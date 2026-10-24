//! Discord transport for Wallis on Twilight: gateway, slash commands, text events, and voice receive/playback.
//! It knows nothing about agents; it turns Discord activity into `Incoming` and exposes `Out` for replies.
pub mod bot;
mod music;
use songbird::driver::DecodeMode;
use songbird::events::{context_data::{DisconnectKind, DisconnectReason}, CoreEvent, Event, EventContext, EventHandler as VoiceHandler};
use songbird::input::{core::io::{MediaSource, ReadOnlySource}, AudioStream, AudioStreamError, Compose, Input, RawAdapter};
use songbird::shards::TwilightMap;
use songbird::{Config as VoiceConfig, Songbird};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use twilight_cache_inmemory::{DefaultInMemoryCache, ResourceType};
use twilight_gateway::{Config, Event as Gateway, EventTypeFlags, Intents, Shard, ShardId, StreamExt as _};
use twilight_http::Client;
use twilight_model::application::interaction::{InteractionData, InteractionType};
use twilight_model::channel::message::{AllowedMentions, MessageFlags};
use twilight_model::gateway::payload::incoming::InteractionCreate;
use twilight_model::http::attachment::Attachment;
use twilight_model::http::interaction::{InteractionResponse, InteractionResponseData, InteractionResponseType};
use twilight_model::id::marker::{ApplicationMarker, GuildMarker, UserMarker};
use twilight_model::id::Id;
use twilight_model::channel::message::Embed;
use twilight_util::builder::command::CommandBuilder;
use twilight_util::builder::embed::{EmbedBuilder, EmbedFooterBuilder, EmbedFieldBuilder};
use wallis_core::utterance::Segmenter;
use wallis_core::media::MAX_SOURCES;

/// Reads Wallis' profile as JSON each time it is asked for, so changes he makes to himself show up at once.
pub type ProfileSource = Arc<dyn Fn() -> Option<serde_json::Value> + std::marker::Send + Sync>;

#[derive(Clone)]
pub struct Settings {
    pub token: String,
    pub data_dir: PathBuf,
    pub write_channels: Vec<String>,
    pub allowed_users: Vec<String>,
    pub read_channels: Vec<String>,
    /// Shown by `/profile`.
    pub profile: ProfileSource,
}

#[derive(Debug)]
pub enum Incoming {
    Ready { bot_id: u64 },
    Mod { guild: u64, channel: u64, user: u64, name: String, options: serde_json::Value, token: String },
    Text { guild: u64, channel: u64, message: u64, user: u64, speaker: String, said: String, direct: bool, media: Vec<String>, reply: Option<u64> },
    MediaUpdate { channel: u64, message: u64, user: u64, media: Vec<String> },
    MediaDelete { channel: u64, messages: Vec<u64> },
    /// A finished utterance of 48 kHz interleaved stereo PCM from an allow-listed speaker.
    Voice { guild: u64, channel: u64, user: u64, speaker: String, pcm: Vec<i16> },
    /// A `Voice` utterance after transcription. The bot makes these off the brain thread; Discord never sends them.
    Heard { guild: u64, channel: u64, user: u64, speaker: String, said: String },
    /// Someone joined, left or moved within Wallis's call. `from`/`to` are voice channel ids (0 = none); only changes touching his channel are sent.
    Presence { guild: u64, user: u64, speaker: String, from: u64, to: u64 },
    /// Includes synthesis startup and mid-stream playback failures.
    SpeechFailed { guild: u64, text: String, tuning: wallis_core::tuning::Tuning, generation: u64, retried: bool, error: String },
}

fn allowed(list: &[String], user: u64) -> bool { list.iter().any(|id| id == "*" || id == &user.to_string()) }

/// Reply side, cheap to clone and usable from any thread.
#[derive(Clone)]
pub struct Out { http: Arc<Client>, songbird: Arc<Songbird>, runtime: tokio::runtime::Handle, cache: Arc<DefaultInMemoryCache>, settings: Settings, send: Send, music: Arc<music::Player>, application: Id<ApplicationMarker> }

// Songbird opens the request when this track starts, so queued speech cannot block interruption.
#[derive(Clone)]
struct Speech { text: String, tuning: Arc<Mutex<wallis_core::tuning::Tuning>>, send: Send, guild: u64, generation: u64, retried: bool, reported: Arc<std::sync::atomic::AtomicBool> }

impl Speech {
    fn report(&self, error: String) {
        if self.reported.swap(true, std::sync::atomic::Ordering::Relaxed) { return; }
        (self.send)(Incoming::SpeechFailed { guild: self.guild, text: self.text.clone(), tuning: self.tuning.lock().expect("speech tuning poisoned").clone(), generation: self.generation, retried: self.retried, error });
    }
}

// Songbird treats next_packet I/O errors as EOF, so report stream failures before it reads them.
struct SpeechStream<R> { stream: R, speech: Speech, bytes: usize, started: std::time::Instant }
impl<R: std::io::Read> std::io::Read for SpeechStream<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self.stream.read(buf) {
            Ok(n) => {
                if self.bytes == 0 && n > 0 { wallis_core::stats::log("tts", format!("first audio {} ({}): {}", wallis_core::stats::secs(self.started.elapsed()), self.speech.tuning.lock().expect("speech tuning poisoned").voice, self.speech.text)); }
                self.bytes += n;
                if n == 0 && !buf.is_empty() && (self.bytes == 0 || !self.bytes.is_multiple_of(4)) { self.speech.report("Speech returned empty or incomplete PCM.".into()); }
                Ok(n)
            }
            Err(error) => {
                if error.kind() != std::io::ErrorKind::Interrupted { self.speech.report(error.to_string()); }
                Err(error)
            }
        }
    }
}

#[async_trait::async_trait]
impl Compose for Speech {
    fn create(&mut self) -> Result<AudioStream<Box<dyn MediaSource>>, AudioStreamError> {
        let tuning = wallis_core::tuning::current();
        *self.tuning.lock().expect("speech tuning poisoned") = tuning.clone();
        let started = std::time::Instant::now();
        let stream = wallis_core::speech::synthesize_stream(&self.text, &tuning).map_err(|error| {
            wallis_core::stats::log("error", format!("tts: {error}"));
            AudioStreamError::Fail(error.into())
        })?;
        // ponytail: speed changes pitch; add time stretching when pitch-preserving speed is required.
        let rate = (24_000.0 / tuning.speed).round() as u32;
        let stream = SpeechStream { stream, speech: self.clone(), bytes: 0, started };
        Ok(AudioStream { input: Box::new(RawAdapter::new(ReadOnlySource::new(stream), rate, 1)) })
    }
    async fn create_async(&mut self) -> Result<AudioStream<Box<dyn MediaSource>>, AudioStreamError> { Err(AudioStreamError::Unsupported) }
    fn should_create_async(&self) -> bool { false }
}

#[async_trait::async_trait]
impl VoiceHandler for Speech {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        let error = match ctx {
            EventContext::Track(tracks) => tracks.first().map(|(state, _)| format!("{:?}", state.playing)).unwrap_or_else(|| "Speech playback failed.".into()),
            _ => "Speech playback failed.".into(),
        };
        self.report(error);
        Some(Event::Cancel)
    }
}

impl Out {
    /// Discord may populate GIF embeds after MESSAGE_CREATE. Refresh off the gateway thread.
    pub fn media(&self, channel: u64, message: u64) -> Vec<String> {
        if !self.settings.read_channels.contains(&channel.to_string()) { return vec![]; }
        self.runtime.block_on(async {
            tokio::time::timeout(std::time::Duration::from_secs(3), async {
                for attempt in 0..3 {
                    let Ok(response) = self.http.message(Id::new(channel), Id::new(message)).await else { return vec![] };
                    let Ok(message) = response.model().await else { return vec![] };
                    if message.author.bot || message.webhook_id.is_some() || !allowed(&self.settings.allowed_users, message.author.id.get()) { return vec![]; }
                    let sources = message_media(&message);
                    if !message.embeds.is_empty() || !message.attachments.is_empty() || attempt == 2 { return sources; }
                    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                }
                vec![]
            }).await.unwrap_or_default()
        })
    }

    /// Last `limit` messages of a read channel, oldest first. Bots and non-allow-listed authors are labelled, not hidden, so he sees the whole room.
    pub fn history(&self, channel: u64, limit: u8) -> Result<String, String> {
        if !self.settings.read_channels.contains(&channel.to_string()) { return Err(format!("Channel {channel} is not in the allowed read channels.")); }
        let messages = self.runtime.block_on(async { let response = self.http.channel_messages(Id::new(channel)).limit(u16::from(limit)).await.map_err(|e| e.to_string())?; response.models().await.map_err(|e| e.to_string()) })?;
        Ok(messages.iter().rev().map(|m| format!("[{}] {}{}: {}{}", m.id, m.author.name, if m.author.bot { " (bot)" } else { "" }, clip(&m.content, 500), if m.attachments.is_empty() { String::new() } else { format!(" [{} attachment(s)]", m.attachments.len()) })).collect::<Vec<_>>().join("\n"))
    }

    pub fn message(&self, channel: u64, text: &str) -> Result<(), String> {
        let none = AllowedMentions::default();
        let request = self.http.create_message(Id::new(channel)).content(text).allowed_mentions(Some(&none));
        self.runtime.block_on(async { request.await }).map(|_| ()).map_err(|error| error.to_string())
    }

    pub fn file(&self, channel: u64, filename: &str, content: &str, text: &str) -> Result<(), String> {
        let attachments = [Attachment::from_bytes(filename.to_string(), content.as_bytes().to_vec(), 1)];
        let none = AllowedMentions::default();
        let request = self.http.create_message(Id::new(channel)).content(text).attachments(&attachments).allowed_mentions(Some(&none));
        self.runtime.block_on(async { request.await }).map(|_| ()).map_err(|error| error.to_string())
    }

    pub fn queue(&self, guild: u64, user: u64, path: PathBuf, title: String) -> Result<(), String> {
        if self.voice_channel(guild).is_none() { self.join(guild, user, None)?; }
        self.runtime.block_on(self.music.queue(guild, path, title))
    }
    pub fn queue_list(&self, guild: u64) -> String { self.runtime.block_on(self.music.list(guild)) }
    pub fn skip_audio(&self, guild: u64) -> Result<(), String> { self.runtime.block_on(self.music.skip(guild)) }
    pub fn stop_audio(&self, guild: u64) -> Result<(), String> { self.runtime.block_on(self.music.stop(guild)) }
    pub fn volume(&self, guild: u64, percent: u32) -> Result<(), String> { self.runtime.block_on(self.music.volume(guild, percent)) }
    pub fn reply_mod(&self, token: &str, text: &str) -> Result<(), String> {
        let client = self.http.interaction(self.application);
        self.runtime.block_on(async { client.update_response(token).content(Some(&clip(text,1900))).allowed_mentions(Some(&AllowedMentions::default())).await }).map(|_| ()).map_err(|e| e.to_string())
    }
    pub fn set_commands(&self) -> Result<(), String> {
        let commands = commands(&self.settings.data_dir)?;
        self.runtime.block_on(async { self.http.interaction(self.application).set_global_commands(&commands).await }).map(|_| ()).map_err(|e| e.to_string())
    }

    /// Queues a live Speech PCM stream. Songbird resamples to Discord's 48 kHz stereo.
    /// Voice, speed and sampling are read when each sentence starts playing, so a tuning change reaches speech already queued.
    pub fn speak(&self, guild: u64, text: &str) -> Result<(), String> {
        self.speak_retry(guild, text, false)
    }

    pub(crate) fn speak_retry(&self, guild: u64, text: &str, retried: bool) -> Result<(), String> {
        let call = self.songbird.get(Id::<GuildMarker>::new(guild)).ok_or("Wallis is not in a voice channel.")?;
        if text.trim().is_empty() || text.chars().count() > 16_000 { return Err("Speech must contain 1–16000 characters.".into()); }
        self.runtime.block_on(async {
            let end = self.music.speaking(guild).await;
            let speech = Speech { text: text.into(), tuning: Arc::new(Mutex::new(wallis_core::tuning::current())), send: self.send.clone(), guild, generation: end.generation, retried, reported: Default::default() };
            let mut track = songbird::tracks::Track::new(Input::Lazy(Box::new(speech.clone())));
            // Register before enqueueing: a lazy source can fail as soon as it starts.
            track.events.add_event(songbird::events::EventData::new(Event::Track(songbird::events::TrackEvent::Error), speech), std::time::Duration::ZERO);
            track.events.add_event(songbird::events::EventData::new(Event::Track(songbird::events::TrackEvent::End), end), std::time::Duration::ZERO);
            call.lock().await.enqueue_with_preload(track, None);
        });
        Ok(())
    }

    /// Joins `channel`, or the voice channel `user` is in, and starts listening. Returns the channel id.
    /// Blocks on the voice handshake, so call it from a worker thread, never from the gateway loop.
    pub fn join(&self, guild: u64, user: u64, channel: Option<u64>) -> Result<u64, String> {
        let guild = Id::<GuildMarker>::new_checked(guild).ok_or("Bad guild id.")?;
        let channel = match channel {
            Some(id) => Id::new_checked(id).ok_or("Bad channel id.")?,
            None => Id::new_checked(user).and_then(|user| self.cache.voice_state(user, guild).map(|state| state.channel_id())).ok_or("They are not in a voice channel. Ask them to join one, or pass `channelId`.")?,
        };
        self.runtime.block_on(async {
            let call = self.songbird.get_or_insert(guild);
            {
                let mut call = call.lock().await;
                if call.current_channel().is_some_and(|current| current.0.get() == channel.get()) { return Ok(channel.get()) }
                let receive = Arc::new(Receive { guild: guild.get(), channel: channel.get(), settings: self.settings.clone(), send: self.send.clone(), ssrc: Mutex::default(), segmenter: Mutex::default(), cache: self.cache.clone(), songbird: self.songbird.clone(), music: self.music.clone(), talking: Mutex::default() });
                // Speaking announcements arrive during the handshake; listen before joining or their SSRCs are lost.
                // Moving between channels keeps the call, so replace its old listeners.
                call.remove_all_global_events();
                call.add_global_event(CoreEvent::SpeakingStateUpdate.into(), SharedReceive(receive.clone()));
                call.add_global_event(CoreEvent::VoiceTick.into(), SharedReceive(receive));
                call.add_global_event(CoreEvent::DriverDisconnect.into(), Rejoin { songbird: self.songbird.clone(), music: self.music.clone() });
            }
            self.songbird.join(guild, channel).await.map_err(|error| format!("Could not join that channel: {error}"))?;
            Ok(channel.get())
        })
    }

    /// Leaves the call once whatever is queued (a goodbye, usually) has been spoken, for at most 20 seconds.
    pub fn leave(&self, guild: u64) -> Result<(), String> {
        self.stop_audio(guild)?;
        let guild = Id::<GuildMarker>::new_checked(guild).ok_or("Bad guild id.")?;
        let call = self.songbird.get(guild).ok_or("You are not in a call.")?;
        self.runtime.block_on(async {
            for _ in 0..80 {
                if call.lock().await.queue().is_empty() { break }
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            }
            self.songbird.remove(guild).await.map_err(|error| format!("Could not leave: {error}"))
        })
    }

    /// Names of everyone (but Wallis) in voice channel `channel` right now, from Discord's voice-state cache.
    pub fn voice_members(&self, channel: u64) -> Vec<(u64, String)> {
        let Some(channel) = Id::new_checked(channel) else { return vec![] };
        let me = self.cache.current_user().map(|user| user.id);
        let mut members: Vec<(u64, String)> = self.cache.voice_channel_states(channel).into_iter().flatten().map(|state| state.user_id()).filter(|id| Some(*id) != me).map(|id| (id.get(), self.name(id.get()))).collect();
        members.sort();
        members
    }

    fn name(&self, user: u64) -> String { Id::<UserMarker>::new_checked(user).and_then(|id| self.cache.user(id).map(|u| u.name.clone())).unwrap_or_else(|| user.to_string()) }

    /// The voice channel Wallis is in, if any.
    /// Sentences queued or being spoken right now.
    pub fn pending_speech(&self, guild: u64) -> usize { self.runtime.block_on(self.music.pending_speech(guild)) }

    pub(crate) fn speech_generation(&self, guild: u64) -> u64 { self.runtime.block_on(self.music.speech_generation(guild)) }

    pub fn voice_channel(&self, guild: u64) -> Option<u64> {
        let call = self.songbird.get(Id::<GuildMarker>::new_checked(guild)?)?;
        self.runtime.block_on(async { call.lock().await.current_channel().map(|channel| channel.0.get()) })
    }

    /// Stops current and queued speech when Wallis chooses to interrupt himself.
    pub fn stop_speaking(&self, guild: u64) {
        self.runtime.block_on(cut_off_speech(&self.songbird, &self.music, guild));
    }
}

type Send = Arc<dyn Fn(Incoming) + std::marker::Send + Sync>;

struct Receive { guild: u64, channel: u64, settings: Settings, send: Send, ssrc: Mutex<HashMap<u32, u64>>, segmenter: Mutex<Segmenter>, cache: Arc<DefaultInMemoryCache>, songbird: Arc<Songbird>, music: Arc<music::Player>, talking: Mutex<HashMap<u32, u64>> }

/// Cuts off current and queued speech (not music), whether Wallis chose to or someone talked over him.
async fn cut_off_speech(songbird: &Songbird, music: &music::Player, guild: u64) {
    music.reset_speech(guild).await;
    if let Some(call) = songbird.get(Id::<GuildMarker>::new(guild)) { call.lock().await.queue().stop(); }
}

/// Discord drops voice connections (voice server moves, expired sessions, network blips). Songbird gives up after its retries
/// but still reports the channel, so `join` would see him as already there and he would sit in a dead call. Rejoin it instead,
/// dropping speech that piled up while the call was down.
struct Rejoin { songbird: Arc<Songbird>, music: Arc<music::Player> }

#[async_trait::async_trait]
impl VoiceHandler for Rejoin {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        let EventContext::DriverDisconnect(data) = ctx else { return None };
        // Leaving and moving are deliberate; a failed first connect is a join attempt (this one or a tool's) that reports its own error.
        if data.kind == DisconnectKind::Connect || matches!(data.reason, None | Some(DisconnectReason::Requested | DisconnectReason::AttemptDiscarded)) { return None; }
        wallis_core::stats::log("error", format!("voice connection lost ({:?}, {:?}); rejoining", data.kind, data.reason));
        let (songbird, music, guild, channel) = (self.songbird.clone(), self.music.clone(), data.guild_id, data.channel_id);
        tokio::spawn(async move {
            let Some(call) = songbird.get(guild) else { return };
            if call.lock().await.current_channel() != Some(channel) { return; }
            cut_off_speech(&songbird, &music, guild.0.get()).await;
            for attempt in 1..=3 {
                let _ = call.lock().await.leave().await;
                // Discord echoes the leave; a join sent before that echo arrives would be cancelled by it.
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                match songbird.join(guild, channel).await {
                    Ok(_) => return wallis_core::stats::log("voice", "rejoined the call"),
                    Err(error) => wallis_core::stats::log("error", format!("voice rejoin {attempt}/3: {error}")),
                }
            }
        });
        None
    }
}

struct SharedReceive(Arc<Receive>);

#[async_trait::async_trait]
impl VoiceHandler for SharedReceive {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        let this = &self.0;
        match ctx {
            EventContext::SpeakingStateUpdate(speaking) => {
                if let Some(user) = speaking.user_id { this.ssrc.lock().expect("ssrc map poisoned").insert(speaking.ssrc, user.0); }
            }
            EventContext::VoiceTick(tick) => {
                let heard: Vec<(u32, &[i16])> = tick.speaking.iter().filter_map(|(ssrc, data)| data.decoded_voice.as_deref().map(|pcm| (*ssrc, pcm))).collect();
                let silent: Vec<u32> = tick.silent.iter().copied().collect();
                // Talking over him long enough (`bargeInMs`, 0 = never) cuts him off; he is told through the transcript that he was interrupted.
                let barge_in = wallis_core::tuning::current().barge_in_ms as u64;
                let talked_over = {
                    let mut talking = this.talking.lock().expect("talking map poisoned");
                    talking.retain(|ssrc, _| heard.iter().any(|(s, _)| s == ssrc));
                    let loud = |pcm: &[i16]| pcm.iter().any(|s| s.saturating_abs() >= 500);
                    heard.iter().any(|(ssrc, pcm)| { let ms = talking.entry(*ssrc).or_default(); *ms = if loud(pcm) { *ms + 20 } else { 0 }; barge_in > 0 && *ms >= barge_in })
                };
                if talked_over && this.music.pending_speech(this.guild).await > 0 {
                    this.talking.lock().expect("talking map poisoned").clear();
                    wallis_core::stats::log("barge", format!("talked over for {barge_in} ms; speech cut off"));
                    cut_off_speech(&this.songbird, &this.music, this.guild).await;
                }
                let finished = this.segmenter.lock().expect("segmenter poisoned").tick(&heard, &silent);
                for utterance in finished {
                    let Some(user) = this.ssrc.lock().expect("ssrc map poisoned").get(&utterance.speaker).copied() else { continue };
                    if !allowed(&this.settings.allowed_users, user) { continue; }
                    let speaker = this.cache.user(Id::<UserMarker>::new(user)).map(|u| u.name.clone()).unwrap_or_else(|| user.to_string());
                    (this.send)(Incoming::Voice { guild: this.guild, channel: this.channel, user, speaker, pcm: utterance.pcm });
                }
            }
            _ => {}
        }
        None
    }
}

fn respond(text: &str, ephemeral: bool) -> InteractionResponse {
    InteractionResponse { kind: InteractionResponseType::ChannelMessageWithSource, data: Some(InteractionResponseData { content: Some(text.to_string()), flags: ephemeral.then_some(MessageFlags::EPHEMERAL), allowed_mentions: Some(AllowedMentions::default()), ..Default::default() }) }
}

const EMBED_COLOR: u32 = 0xD6336C;

/// Clips to what Discord accepts in an embed field, on a character boundary.
fn clip(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max { text.to_string() } else { format!("{}…", text.chars().take(max - 1).collect::<String>().trim_end()) }
}

/// The `/profile` card: name, tagline and bio on top, the short facts side by side, then the longer ones.
/// Missing or empty fields are left out, so an older profile file still renders.
pub fn profile_embed(profile: &serde_json::Value) -> Embed {
    let text = |key: &str| profile.get(key).and_then(serde_json::Value::as_str).map(str::trim).filter(|value| !value.is_empty());
    let mut embed = EmbedBuilder::new().color(EMBED_COLOR).title(clip(text("name").unwrap_or("Wallis"), 256));
    let description = [text("tagline").map(|tagline| format!("*{}*", clip(tagline, 200))), text("bio").map(|bio| clip(bio, 1800))].into_iter().flatten().collect::<Vec<_>>().join("\n\n");
    if !description.is_empty() { embed = embed.description(description); }
    for (label, key) in [("Age", "age"), ("Pronouns", "pronouns"), ("Location", "location"), ("Occupation", "occupation")] {
        if let Some(value) = text(key) { embed = embed.field(EmbedFieldBuilder::new(label, clip(value, 200)).inline()); }
    }
    let interests = profile.get("interests").and_then(serde_json::Value::as_array).map(|list| list.iter().filter_map(serde_json::Value::as_str).collect::<Vec<_>>().join(" · ")).unwrap_or_default();
    if !interests.is_empty() { embed = embed.field(EmbedFieldBuilder::new("Interests", clip(&interests, 1000))); }
    for (label, key) in [("Looking for", "lookingFor"), ("Fun fact", "funFact")] {
        if let Some(value) = text(key) { embed = embed.field(EmbedFieldBuilder::new(label, clip(value, 1000))); }
    }
    embed.footer(EmbedFooterBuilder::new("Wallis")).build()
}

fn respond_embed(embed: Embed) -> InteractionResponse {
    InteractionResponse { kind: InteractionResponseType::ChannelMessageWithSource, data: Some(InteractionResponseData { embeds: Some(vec![embed]), allowed_mentions: Some(AllowedMentions::default()), ..Default::default() }) }
}

struct Bot { settings: Settings, send: Send, http: Arc<Client>, cache: Arc<DefaultInMemoryCache>, application: Id<ApplicationMarker> }

/// Prefer Discord's resolved video/image assets over a provider's HTML landing page.
fn message_media(message: &twilight_model::channel::Message) -> Vec<String> {
    let mut urls = Vec::new();
    let mut add = |url: &str| {
        if urls.len() < MAX_SOURCES && !urls.iter().any(|source: &String| source == url) { urls.push(url.to_string()); }
    };
    for attachment in &message.attachments { add(&attachment.url); }
    for embed in &message.embeds {
        if let Some(video) = &embed.video { if let Some(url) = video.proxy_url.as_ref().or(video.url.as_ref()) { add(url); continue; } }
        if let Some(image) = &embed.image { add(image.proxy_url.as_deref().unwrap_or(&image.url)); }
        else if let Some(thumbnail) = &embed.thumbnail { add(thumbnail.proxy_url.as_deref().unwrap_or(&thumbnail.url)); }
    }
    // Discord often supplies the embed later. Direct assets can be fetched immediately.
    for (start, _) in message.content.match_indices("https://") {
        let url = message.content[start..].split(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | ')' | ']' | '"')).next().unwrap_or("").trim_end_matches([',', '.', ';', '!']);
        if url.starts_with("https://") && !message.embeds.iter().any(|embed| embed.url.as_deref() == Some(url) && (embed.video.is_some() || embed.image.is_some() || embed.thumbnail.is_some())) { add(url); }
    }
    urls
}

impl Bot {
    async fn message(&self, message: &twilight_model::gateway::payload::incoming::MessageCreate) {
        let Some(guild) = message.guild_id else { return };
        if message.author.bot || message.webhook_id.is_some() { return; }
        if !allowed(&self.settings.allowed_users, message.author.id.get()) || !self.settings.read_channels.contains(&message.channel_id.to_string()) { return; }
        let media = message_media(message);
        if message.content.trim().is_empty() && media.is_empty() { return; }
        let Some(me) = self.cache.current_user().map(|user| user.id) else { return };
        let replied_to_me = message.referenced_message.as_ref().is_some_and(|reference| reference.author.id == me);
        let direct = message.mentions.iter().any(|mention| mention.id == me) || replied_to_me || wallis_core::participation::names_wallis(&message.content);
        let speaker = message.member.as_ref().and_then(|member| member.nick.clone()).unwrap_or_else(|| message.author.name.clone());
        if let Some(reference) = &message.referenced_message {
            if !reference.author.bot && allowed(&self.settings.allowed_users, reference.author.id.get()) && reference.channel_id == message.channel_id {
                (self.send)(Incoming::MediaUpdate { channel: reference.channel_id.get(), message: reference.id.get(), user: reference.author.id.get(), media: message_media(reference) });
            }
        }
        (self.send)(Incoming::Text { guild: guild.get(), channel: message.channel_id.get(), message: message.id.get(), user: message.author.id.get(), speaker, said: message.content.clone(), direct, media, reply: message.reference.as_ref().and_then(|reference| reference.message_id).map(|id| id.get()) });
    }

    fn media_update(&self, message: &twilight_model::channel::Message) {
        if message.author.bot || message.webhook_id.is_some() || !allowed(&self.settings.allowed_users, message.author.id.get()) || !self.settings.read_channels.contains(&message.channel_id.to_string()) { return; }
        (self.send)(Incoming::MediaUpdate { channel: message.channel_id.get(), message: message.id.get(), user: message.author.id.get(), media: message_media(message) });
    }

    async fn command(&self, interaction: &InteractionCreate) {
        if interaction.kind != InteractionType::ApplicationCommand { return }
        let Some(InteractionData::ApplicationCommand(data)) = &interaction.data else { return };
        let Some(user) = interaction.author_id() else { return };
        let client = self.http.interaction(self.application);
        let client = &client;
        let reply = |text: &'static str, ephemeral| async move { client.create_response(interaction.id, &interaction.token, &respond(text, ephemeral)).await };
        if !allowed(&self.settings.allowed_users, user.get()) {
            let _ = reply("You are not on this Wallis' allow-list.", true).await;
            return;
        }
        if data.name == "profile" {
            let response = match (self.settings.profile)() { Some(profile) => respond_embed(profile_embed(&profile)), None => respond("Wallis has no profile yet.", true) };
            let _ = client.create_response(interaction.id, &interaction.token, &response).await;
        } else if let Some(guild) = interaction.guild_id {
            let channel = interaction.channel.as_ref().map(|c| c.id.get()).unwrap_or(0);
            if !self.settings.write_channels.contains(&channel.to_string()) { let _ = reply("This channel is not an allowed output channel.", true).await; return; }
            let deferred = InteractionResponse { kind: InteractionResponseType::DeferredChannelMessageWithSource, data: None };
            if client.create_response(interaction.id, &interaction.token, &deferred).await.is_err() { return; }
            let mut options = serde_json::Map::new();
            for option in &data.options {
                use twilight_model::application::interaction::application_command::CommandOptionValue;
                let value = match &option.value { CommandOptionValue::String(s) => serde_json::json!(s), CommandOptionValue::Integer(n) => serde_json::json!(n), CommandOptionValue::Boolean(b) => serde_json::json!(b), _ => continue };
                options.insert(option.name.clone(), value);
            }
            (self.send)(Incoming::Mod { guild: guild.get(), channel, user: user.get(), name: data.name.clone(), options: options.into(), token: interaction.token.clone() });
        }
    }
}

fn commands(data: &std::path::Path) -> Result<Vec<twilight_model::application::command::Command>, String> {
    use twilight_model::application::command::{Command, CommandType};
    let mut commands: Vec<Command> = vec![CommandBuilder::new("profile", "Show who Wallis is", CommandType::ChatInput).build()];
    for m in wallis_core::mods::scan(data)?.into_iter().filter(|m| m.enabled) {
        // Serde uses the exact Discord command schema and avoids a separate option-builder for each scalar type.
        let options: Vec<_> = m.options.iter().map(|o| serde_json::json!({"name":o.name,"description":o.description,"type":match o.kind.as_str() {"string"=>3,"integer"=>4,_=>5},"required":o.required})).collect();
        let mut command = CommandBuilder::new(m.name,m.description,CommandType::ChatInput).build();
        command.options = serde_json::from_value(serde_json::json!(options)).map_err(|e| e.to_string())?;
        commands.push(command);
    }
    Ok(commands)
}

/// Connects and runs until the gateway closes. `on_event` is called from Discord's async tasks and must not block.
pub async fn run(settings: Settings, ready: impl FnOnce(Out) + std::marker::Send + 'static, on_event: impl Fn(Incoming) + std::marker::Send + Sync + 'static) -> Result<(), String> {
    let http = Arc::new(Client::new(settings.token.clone()));
    let me = http.current_user().await.map_err(|error| format!("Discord login failed: {error}"))?.model().await.map_err(|error| format!("Discord login failed: {error}"))?;
    let application = http.current_user_application().await.map_err(|error| error.to_string())?.model().await.map_err(|error| error.to_string())?.id;
    let intents = Intents::GUILDS | Intents::GUILD_VOICE_STATES | Intents::GUILD_MESSAGES | Intents::MESSAGE_CONTENT;
    let mut shard = Shard::with_config(ShardId::ONE, Config::new(settings.token.clone(), intents));
    let songbird = Arc::new(Songbird::twilight_from_config(Arc::new(TwilightMap::new(HashMap::from([(0, shard.sender())]))), me.id, VoiceConfig::default().decode_mode(DecodeMode::Decode(Default::default()))));
    let cache = Arc::new(DefaultInMemoryCache::builder().resource_types(ResourceType::USER_CURRENT | ResourceType::USER | ResourceType::VOICE_STATE | ResourceType::GUILD).build());
    let commands = commands(&settings.data_dir)?;
    http.interaction(application).set_global_commands(&commands).await.map_err(|error| format!("Unable to register commands: {error}"))?;
    let send: Send = Arc::new(on_event);
    let music = music::Player::new(songbird.clone());
    ready(Out { music, application, http: http.clone(), songbird: songbird.clone(), runtime: tokio::runtime::Handle::current(), cache: cache.clone(), settings: settings.clone(), send: send.clone() });
    let bot = Arc::new(Bot { settings, send, http, cache: cache.clone(), application });
    while let Some(item) = shard.next_event(EventTypeFlags::all()).await {
        let event = match item {
            Ok(event) => event,
            Err(_) => continue,
        };
        // The cache overwrites the old channel, so read it before updating.
        let before = if let Gateway::VoiceStateUpdate(update) = &event { update.guild_id.map(|guild| (guild, cache.voice_state(update.user_id, guild).map(|state| state.channel_id().get()).unwrap_or(0))) } else { None };
        cache.update(&event);
        songbird.process(&event).await;
        if let (Gateway::VoiceStateUpdate(update), Some((guild, from))) = (&event, before) {
            let (user, to) = (update.user_id.get(), update.channel_id.map(|id| id.get()).unwrap_or(0));
            let mine = match songbird.get(guild) { Some(call) => call.lock().await.current_channel().map(|channel| channel.0.get()), None => None };
            if update.user_id != me.id && from != to && mine.is_some_and(|mine| mine == from || mine == to) && allowed(&bot.settings.allowed_users, user) {
                let speaker = update.member.as_ref().and_then(|member| member.nick.clone()).unwrap_or_else(|| update.member.as_ref().map(|member| member.user.name.clone()).unwrap_or_else(|| user.to_string()));
                (bot.send)(Incoming::Presence { guild: guild.get(), user, speaker, from, to });
            }
        }
        match event {
            Gateway::Ready(ready) => (bot.send)(Incoming::Ready { bot_id: ready.user.id.get() }),
            Gateway::MessageCreate(message) => bot.message(&message).await,
            Gateway::MessageUpdate(message) => bot.media_update(&message),
            Gateway::MessageDelete(message) => (bot.send)(Incoming::MediaDelete { channel: message.channel_id.get(), messages: vec![message.id.get()] }),
            Gateway::MessageDeleteBulk(message) => (bot.send)(Incoming::MediaDelete { channel: message.channel_id.get(), messages: message.ids.iter().map(|id| id.get()).collect() }),
            Gateway::InteractionCreate(interaction) => { let bot = bot.clone(); tokio::spawn(async move { bot.command(&interaction).await }); }
            _ => {}
        }
    }
    // The stream only ends when Discord closes the session for good (bad token, missing intents).
    Err("Discord closed the connection (check the bot token and its Message Content intent).".into())
}

#[cfg(test)]
mod profile_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn enabled_mods_become_valid_discord_commands() {
        let root = std::env::temp_dir().join(format!("wallis-commands-{}",std::process::id()));
        std::fs::create_dir_all(root.join("mods/play")).unwrap();
        std::fs::write(root.join("mods/play/mod.json"),include_str!("../../../mods/play/mod.json")).unwrap();
        std::fs::write(root.join("mods/play/index.ts"),"").unwrap();
        let list = commands(&root).unwrap();
        assert_eq!(list.len(),2);
        assert_eq!(list[1].options[0].name,"query");
        std::fs::write(root.join("mods.json"),r#"{"play":false}"#).unwrap();
        assert_eq!(commands(&root).unwrap().len(),1);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn the_profile_card_has_the_facts_in_order_and_skips_empty_ones() {
        let embed = profile_embed(&json!({ "name": "Wallis Reed", "tagline": "tea first", "bio": "Hi.", "age": "27", "pronouns": "", "location": "Leeds", "interests": ["tea", "trains"], "funFact": "x".repeat(5000) }));
        assert_eq!((embed.title.as_deref(), embed.description.as_deref()), (Some("Wallis Reed"), Some("*tea first*\n\nHi.")));
        let fields: Vec<_> = embed.fields.iter().map(|field| (field.name.as_str(), field.inline)).collect();
        assert_eq!(fields, [("Age", true), ("Location", true), ("Interests", false), ("Fun fact", false)]);
        assert_eq!(embed.fields[2].value, "tea · trains");
        assert!(embed.fields[3].value.chars().count() <= 1000);
    }

    #[test]
    fn an_empty_profile_still_renders() {
        let embed = profile_embed(&json!({}));
        assert_eq!((embed.title.as_deref(), embed.description, embed.fields.len()), (Some("Wallis"), None, 0));
    }
}

#[cfg(test)]
mod speech_tests {
    use super::*;
    use std::io::Write;

    #[tokio::test]
    async fn speaker_announcements_identify_later_utterances() {
        let songbird = Arc::new(Songbird::twilight(Arc::new(TwilightMap::new(HashMap::new())), Id::new(1)));
        let receive = Arc::new(Receive {
            guild: 1, channel: 2,
            settings: Settings { token: String::new(), data_dir: std::env::temp_dir(), allowed_users: vec!["42".into()], read_channels: vec![], write_channels: vec![], profile: Arc::new(|| None) },
            send: Arc::new(|_| {}), ssrc: Mutex::default(), segmenter: Mutex::default(), cache: Arc::new(DefaultInMemoryCache::new()),
            songbird: songbird.clone(), music: music::Player::new(songbird), talking: Mutex::default(),
        });
        let speaking = serde_json::from_value(serde_json::json!({ "speaking": 1, "ssrc": 7, "user_id": "42" })).unwrap();
        SharedReceive(receive.clone()).act(&EventContext::SpeakingStateUpdate(speaking)).await;
        let mut segmenter = receive.segmenter.lock().unwrap();
        for _ in 0..20 { assert!(segmenter.tick(&[(7, &[2000; 1920])], &[]).is_empty()); }
        let utterances: Vec<_> = (0..8).flat_map(|_| segmenter.tick(&[], &[7])).collect();
        assert_eq!(utterances.len(), 1);
        let user = receive.ssrc.lock().unwrap()[&utterances[0].speaker];
        assert_eq!(user, 42);
        assert!(allowed(&receive.settings.allowed_users, user));
        assert!(!allowed(&receive.settings.allowed_users, 43));
    }

    #[tokio::test]
    async fn playback_errors_report_the_actual_tuning_and_retry_budget() {
        struct Broken;
        impl std::io::Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> { Err(std::io::Error::other("broken speech stream")) }
        }
        for prefix in [0, 24_000] {
            let (send, mut receive) = tokio::sync::mpsc::unbounded_channel();
            let failed = wallis_core::tuning::Tuning { voice: "test-voice".into(), temp: 0.8, ..Default::default() };
            let speech = Speech { text: "Keep this reply".into(), tuning: Arc::new(Mutex::new(failed.clone())), send: Arc::new(move |event| { let _ = send.send(event); }), guild: 1, generation: 7, retried: prefix > 0, reported: Default::default() };
            let source = std::io::Read::chain(std::io::Read::take(std::io::repeat(0), prefix), Broken);
            let mut stream = SpeechStream { stream: source, speech: speech.clone(), bytes: 0, started: std::time::Instant::now() };
            let mut audio = Vec::new();
            assert!(std::io::Read::read_to_end(&mut stream, &mut audio).is_err());
            assert_eq!(audio.len(), prefix as usize);
            let event = receive.try_recv().unwrap();
            let Incoming::SpeechFailed { guild, text, tuning, generation, retried, .. } = event else { panic!("expected speech failure") };
            assert_eq!((guild, text.as_str(), tuning, generation, retried), (1, "Keep this reply", failed, 7, prefix > 0));
            speech.act(&EventContext::Track(&[])).await;
            assert!(receive.try_recv().is_err(), "a stream error and track error must trigger only one recovery");
        }
    }

    #[test]
    fn songbird_decodes_the_download_formats() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        for name in ["tone.webm", "tone.m4a"] {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name);
            let input: Input = songbird::input::File::new(path).into();
            let mut input = runtime.block_on(input.make_playable_async(songbird::input::codecs::get_codec_registry(),songbird::input::codecs::get_probe())).unwrap();
            let parsed = input.parsed_mut().unwrap();
            let packet = parsed.format.next_packet().unwrap();
            assert!(parsed.decoder.decode(&packet).unwrap().frames() > 0,"{name}");
        }
    }

    #[test]
    fn songbird_decodes_live_speech_pcm_before_the_stream_ends() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let socket = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        socket.set_read_timeout(Some(std::time::Duration::from_secs(2))).unwrap();
        let (done, wait) = std::sync::mpsc::channel();
        let producer = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket.write_all(&0.25f32.to_le_bytes().repeat(2400)).unwrap();
            // No EOF until the decoder has consumed a packet.
            let _ = wait.recv_timeout(std::time::Duration::from_secs(3));
        });
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let input: Input = RawAdapter::new(ReadOnlySource::new(socket), 24_000, 1).into();
        let mut playable = input.make_playable(songbird::input::codecs::get_codec_registry(), songbird::input::codecs::get_probe(), runtime.handle()).unwrap();
        let parsed = playable.parsed_mut().unwrap();
        let packet = parsed.format.next_packet().unwrap();
        let audio = parsed.decoder.decode(&packet).unwrap();
        assert_eq!((audio.spec().rate, audio.spec().channels.count(), audio.frames()), (24_000, 1, 480));
        done.send(()).unwrap();
        producer.join().unwrap();
    }
}

#[cfg(test)]
mod media_tests {
    use super::*;
    use serde_json::json;

    fn message(content: &str, attachments: serde_json::Value, embeds: serde_json::Value) -> twilight_model::channel::Message {
        serde_json::from_value(json!({
            "id":"10", "guild_id":"1", "channel_id":"2", "type":0,
            "author":{"id":"3", "username":"Jake", "discriminator":"0", "avatar":null},
            "content":content, "attachments":attachments, "embeds":embeds,
            "timestamp":"2026-10-14T00:00:00.000000+00:00", "edited_timestamp":null,
            "tts":false, "mention_everyone":false, "mentions":[], "mention_roles":[], "pinned":false
        })).unwrap()
    }

    #[tokio::test]
    async fn uploads_gif_embeds_and_pasted_links_survive_discord_intake() {
        let cache = Arc::new(DefaultInMemoryCache::new());
        let ready: twilight_model::gateway::payload::incoming::Ready = serde_json::from_value(json!({
            "application":{"id":"4", "flags":0}, "guilds":[], "resume_gateway_url":"wss://gateway.discord.gg",
            "session_id":"test", "v":10,
            "user":{"id":"4", "username":"Wallis", "discriminator":"0", "avatar":null, "bot":true, "mfa_enabled":false}
        })).unwrap();
        cache.update(&Gateway::Ready(ready));
        let events = Arc::new(Mutex::new(Vec::new()));
        let received = events.clone();
        let bot = Bot {
            settings: Settings { token: String::new(), data_dir: PathBuf::new(), write_channels: vec![], allowed_users: vec!["3".into()], read_channels: vec!["2".into()], profile: Arc::new(|| None) },
            send: Arc::new(move |event| received.lock().unwrap().push(event)),
            http: Arc::new(Client::new("test".into())), cache, application: Id::new(4),
        };
        let upload = message("", json!([{"id":"5", "filename":"image.png", "size":100,
            "url":"https://cdn.discordapp.com/image.png?ex=signed", "proxy_url":"https://media.discordapp.net/image.png"}]), json!([]));
        let gif = message("https://tenor.com/view/cat", json!([]), json!([{"type":"gifv", "url":"https://tenor.com/view/cat",
            "video":{"url":"https://media.tenor.com/cat.mp4"}, "thumbnail":{"url":"https://media.tenor.com/cat.png"}}]));
        let link = message("here it is wallis: [the clip](https://images-ext-1.discordapp.net/external/cat.mp4)", json!([]), json!([]));
        for message in [&upload, &gif, &link] {
            bot.message(&twilight_model::gateway::payload::incoming::MessageCreate(message.clone())).await;
        }
        {
            let guard = events.lock().unwrap();
            assert_eq!(guard.len(), 3, "media-only uploads must not be dropped");
            for (event, url) in guard.iter().zip(["https://cdn.discordapp.com/image.png?ex=signed", "https://media.tenor.com/cat.mp4", "https://images-ext-1.discordapp.net/external/cat.mp4"]) {
                let Incoming::Text { media, .. } = event else { panic!("expected text event") };
                assert_eq!(media, &[url.to_string()]);
            }
        }
        let mut denied = upload.clone();
        denied.author.id = Id::new(99);
        bot.message(&twilight_model::gateway::payload::incoming::MessageCreate(denied)).await;
        let mut denied = upload;
        denied.channel_id = Id::new(99);
        bot.message(&twilight_model::gateway::payload::incoming::MessageCreate(denied)).await;
        assert_eq!(events.lock().unwrap().len(), 3, "media must not bypass access rules");
    }
}
