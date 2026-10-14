//! Discord transport for Wallis on Twilight: gateway, slash commands, text events, and voice receive/playback.
//! It knows nothing about agents; it turns Discord activity into `Incoming` and exposes `Out` for replies.
pub mod bot;
use songbird::driver::DecodeMode;
use songbird::events::{CoreEvent, Event, EventContext, EventHandler as VoiceHandler};
use songbird::input::{Input, RawAdapter};
use songbird::shards::TwilightMap;
use songbird::{Config as VoiceConfig, Songbird};
use std::collections::HashMap;
use std::io::Cursor;
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
use twilight_util::builder::command::CommandBuilder;
use wallis_core::utterance::Segmenter;

#[derive(Clone, Debug)]
pub struct Settings {
    pub token: String,
    pub allowed_users: Vec<String>,
    pub read_channels: Vec<String>,
    /// Shown by `/profile`.
    pub profile: String,
}

#[derive(Debug)]
pub enum Incoming {
    Ready { bot_id: u64 },
    Text { guild: u64, channel: u64, message: u64, user: u64, speaker: String, said: String, direct: bool },
    /// A finished utterance of 48 kHz interleaved stereo PCM from an allow-listed speaker.
    Voice { guild: u64, channel: u64, user: u64, speaker: String, pcm: Vec<i16> },
    Joined { guild: u64, channel: u64 },
    Left { guild: u64 },
}

fn allowed(list: &[String], user: u64) -> bool { list.iter().any(|id| id == "*" || id == &user.to_string()) }

/// Reply side, cheap to clone and usable from any thread.
#[derive(Clone)]
pub struct Out { http: Arc<Client>, songbird: Arc<Songbird>, runtime: tokio::runtime::Handle }

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

    /// Queues 48 kHz stereo PCM on the guild's call; returns once playback has been handed off.
    pub fn play(&self, guild: u64, pcm: &[i16]) -> Result<(), String> {
        let call = self.songbird.get(Id::<GuildMarker>::new(guild)).ok_or("Wallis is not in a voice channel.")?;
        let bytes: Vec<u8> = pcm.iter().flat_map(|&s| (s as f32 / 32768.0).to_le_bytes()).collect();
        let input: Input = RawAdapter::new(Cursor::new(bytes), 48_000, 2).into();
        self.runtime.block_on(async { call.lock().await.enqueue_input(input).await });
        Ok(())
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

struct Bot { settings: Settings, send: Send, http: Arc<Client>, cache: Arc<DefaultInMemoryCache>, songbird: Arc<Songbird>, application: Id<ApplicationMarker> }

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
        let Some(guild) = interaction.guild_id else { return };
        match data.name.as_str() {
            "leave" => {
                let _ = self.songbird.remove(guild).await;
                (self.send)(Incoming::Left { guild: guild.get() });
                let _ = reply("Later.", false).await;
            }
            "join" => {
                let channel = self.cache.voice_state(user, guild).map(|state| state.channel_id());
                let Some(channel) = channel else {
                    let _ = reply("Join a voice channel first.", true).await;
                    return;
                };
                let deferred = InteractionResponse { kind: InteractionResponseType::DeferredChannelMessageWithSource, data: None };
                let _ = client.create_response(interaction.id, &interaction.token, &deferred).await;
                let text = match self.songbird.join(guild, channel).await {
                    Ok(call) => {
                        let receive = Arc::new(Receive { guild: guild.get(), channel: channel.get(), settings: self.settings.clone(), send: self.send.clone(), ssrc: Mutex::default(), segmenter: Mutex::default(), cache: self.cache.clone() });
                        let mut call = call.lock().await;
                        call.add_global_event(CoreEvent::SpeakingStateUpdate.into(), SharedReceive(receive.clone()));
                        call.add_global_event(CoreEvent::VoiceTick.into(), SharedReceive(receive));
                        (self.send)(Incoming::Joined { guild: guild.get(), channel: channel.get() });
                        "Joined.".to_string()
                    }
                    Err(error) => format!("Could not join that channel: {error}"),
                };
                let _ = client.update_response(&interaction.token).content(Some(&text)).await;
            }
            "profile" => { let _ = client.create_response(interaction.id, &interaction.token, &respond(&self.settings.profile, false)).await; }
            _ => {}
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
    let commands = [("join", "Bring Wallis into your voice channel"), ("leave", "Send Wallis away"), ("profile", "Show who Wallis is")].map(|(name, description)| CommandBuilder::new(name, description, twilight_model::application::command::CommandType::ChatInput).build());
    http.interaction(application).set_global_commands(&commands).await.map_err(|error| format!("Unable to register commands: {error}"))?;
    ready(Out { http: http.clone(), songbird: songbird.clone(), runtime: tokio::runtime::Handle::current() });
    let bot = Arc::new(Bot { settings, send: Arc::new(on_event), http, cache: cache.clone(), songbird: songbird.clone(), application });
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
            // Off the gateway loop: /join waits on the voice handshake, which needs this loop to keep delivering events.
            Gateway::InteractionCreate(interaction) => { let bot = bot.clone(); tokio::spawn(async move { bot.command(&interaction).await }); }
            _ => {}
        }
    }
    // The stream only ends when Discord closes the session for good (bad token, missing intents).
    Err("Discord closed the connection (check the bot token and its Message Content intent).".into())
}

