//! Discord transport for Wallis on Twilight: gateway, slash commands, text events, and voice receive/playback.
//! It knows nothing about agents; it turns Discord activity into `Incoming` and exposes `Out` for replies.
pub mod bot;
use songbird::driver::DecodeMode;
use songbird::events::{CoreEvent, Event, EventContext, EventHandler as VoiceHandler};
use songbird::input::{core::io::{MediaSource, ReadOnlySource}, AudioStream, AudioStreamError, Compose, Input, RawAdapter};
use songbird::shards::TwilightMap;
use songbird::{Config as VoiceConfig, Songbird};
use std::collections::HashMap;
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

/// Reads Wallis' profile as JSON each time it is asked for, so changes he makes to himself show up at once.
pub type ProfileSource = Arc<dyn Fn() -> Option<serde_json::Value> + std::marker::Send + Sync>;

#[derive(Clone)]
pub struct Settings {
    pub token: String,
    pub allowed_users: Vec<String>,
    pub read_channels: Vec<String>,
    /// Shown by `/profile`.
    pub profile: ProfileSource,
}

#[derive(Debug)]
pub enum Incoming {
    Ready { bot_id: u64 },
    Text { guild: u64, channel: u64, message: u64, user: u64, speaker: String, said: String, direct: bool },
    /// A finished utterance of 48 kHz interleaved stereo PCM from an allow-listed speaker.
    Voice { guild: u64, channel: u64, user: u64, speaker: String, pcm: Vec<i16> },
}

fn allowed(list: &[String], user: u64) -> bool { list.iter().any(|id| id == "*" || id == &user.to_string()) }

/// Reply side, cheap to clone and usable from any thread.
#[derive(Clone)]
pub struct Out { http: Arc<Client>, songbird: Arc<Songbird>, runtime: tokio::runtime::Handle, cache: Arc<DefaultInMemoryCache>, settings: Settings, send: Send }

// Songbird opens the request when this track starts, so queued speech cannot block interruption.
struct Pocket { text: String, rate: u32 }

#[async_trait::async_trait]
impl Compose for Pocket {
    fn create(&mut self) -> Result<AudioStream<Box<dyn MediaSource>>, AudioStreamError> {
        let stream = wallis_core::speech::synthesize_stream(&self.text).map_err(|error| {
            eprintln!("wallis: {error}");
            AudioStreamError::Fail(error.into())
        })?;
        Ok(AudioStream { input: Box::new(RawAdapter::new(ReadOnlySource::new(stream), self.rate, 1)) })
    }
    async fn create_async(&mut self) -> Result<AudioStream<Box<dyn MediaSource>>, AudioStreamError> { Err(AudioStreamError::Unsupported) }
    fn should_create_async(&self) -> bool { false }
}

impl Out {
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

    /// Queues a live Pocket PCM stream. Songbird resamples to Discord's 48 kHz stereo.
    pub fn speak(&self, guild: u64, text: &str, length_scale: f32) -> Result<(), String> {
        if !length_scale.is_finite() || !(0.5..=2.0).contains(&length_scale) { return Err("Invalid speaking speed.".into()); }
        let call = self.songbird.get(Id::<GuildMarker>::new(guild)).ok_or("Wallis is not in a voice channel.")?;
        if text.trim().is_empty() || text.chars().count() > 16_000 { return Err("Speech must contain 1–16000 characters.".into()); }
        // ponytail: speed changes pitch; add time stretching when pitch-preserving speed is required.
        let input = Input::Lazy(Box::new(Pocket { text: text.into(), rate: (24_000.0 / length_scale).round() as u32 }));
        self.runtime.block_on(async { call.lock().await.enqueue_with_preload(input.into(), None) });
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
            if let Some(call) = self.songbird.get(guild) {
                if call.lock().await.current_channel().is_some_and(|current| current.0.get() == channel.get()) { return Ok(channel.get()) }
            }
            let call = self.songbird.join(guild, channel).await.map_err(|error| format!("Could not join that channel: {error}"))?;
            let receive = Arc::new(Receive { guild: guild.get(), channel: channel.get(), settings: self.settings.clone(), send: self.send.clone(), ssrc: Mutex::default(), segmenter: Mutex::default(), cache: self.cache.clone() });
            let mut call = call.lock().await;
            // Moving between channels keeps the call, so drop the old listeners rather than hearing everyone twice.
            call.remove_all_global_events();
            call.add_global_event(CoreEvent::SpeakingStateUpdate.into(), SharedReceive(receive.clone()));
            call.add_global_event(CoreEvent::VoiceTick.into(), SharedReceive(receive));
            Ok(channel.get())
        })
    }

    /// Leaves the call once whatever is queued (a goodbye, usually) has been spoken, for at most 20 seconds.
    pub fn leave(&self, guild: u64) -> Result<(), String> {
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

    /// The voice channel Wallis is in, if any.
    pub fn voice_channel(&self, guild: u64) -> Option<u64> {
        let call = self.songbird.get(Id::<GuildMarker>::new_checked(guild)?)?;
        self.runtime.block_on(async { call.lock().await.current_channel().map(|channel| channel.0.get()) })
    }

    /// Drops everything queued (used when the user interrupts).
    pub fn stop_speaking(&self, guild: u64) {
        if let Some(call) = self.songbird.get(Id::<GuildMarker>::new(guild)) { self.runtime.block_on(async { call.lock().await.queue().stop() }); }
    }
}

type Send = Arc<dyn Fn(Incoming) + std::marker::Send + Sync>;

struct Receive { guild: u64, channel: u64, settings: Settings, send: Send, ssrc: Mutex<HashMap<u32, u64>>, segmenter: Mutex<Segmenter>, cache: Arc<DefaultInMemoryCache> }

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

impl Bot {
    async fn message(&self, message: &twilight_model::gateway::payload::incoming::MessageCreate) {
        let Some(guild) = message.guild_id else { return };
        if message.author.bot || message.webhook_id.is_some() || message.content.trim().is_empty() { return; }
        if !allowed(&self.settings.allowed_users, message.author.id.get()) || !self.settings.read_channels.contains(&message.channel_id.to_string()) { return; }
        let Some(me) = self.cache.current_user().map(|user| user.id) else { return };
        let replied_to_me = message.referenced_message.as_ref().is_some_and(|reference| reference.author.id == me);
        let direct = message.mentions.iter().any(|mention| mention.id == me) || replied_to_me || wallis_core::participation::names_wallis(&message.content);
        let speaker = message.member.as_ref().and_then(|member| member.nick.clone()).unwrap_or_else(|| message.author.name.clone());
        (self.send)(Incoming::Text { guild: guild.get(), channel: message.channel_id.get(), message: message.id.get(), user: message.author.id.get(), speaker, said: message.content.clone(), direct });
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
        }
    }
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
    // Joining and leaving are conversation now; /profile is the only command.
    let commands = [("profile", "Show who Wallis is")].map(|(name, description)| CommandBuilder::new(name, description, twilight_model::application::command::CommandType::ChatInput).build());
    http.interaction(application).set_global_commands(&commands).await.map_err(|error| format!("Unable to register commands: {error}"))?;
    let send: Send = Arc::new(on_event);
    ready(Out { http: http.clone(), songbird: songbird.clone(), runtime: tokio::runtime::Handle::current(), cache: cache.clone(), settings: settings.clone(), send: send.clone() });
    let bot = Arc::new(Bot { settings, send, http, cache: cache.clone(), application });
    while let Some(item) = shard.next_event(EventTypeFlags::all()).await {
        let event = match item {
            Ok(event) => event,
            Err(_) => continue,
        };
        cache.update(&event);
        songbird.process(&event).await;
        match event {
            Gateway::Ready(ready) => (bot.send)(Incoming::Ready { bot_id: ready.user.id.get() }),
            Gateway::MessageCreate(message) => bot.message(&message).await,
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

    #[test]
    fn songbird_decodes_live_pocket_pcm_before_the_stream_ends() {
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
