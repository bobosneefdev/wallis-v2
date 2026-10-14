//! Discord transport for Wallis: gateway, slash commands, text events, and voice receive/playback.
//! It knows nothing about agents; it turns Discord activity into `Incoming` and exposes `Out` for replies.
use serenity::all::{
    ChannelId, Client, Command, Context, CreateAttachment, CreateCommand, CreateInteractionResponse, CreateInteractionResponseMessage,
    CreateMessage, EventHandler, GatewayIntents, GuildId, Interaction, Message, Ready, UserId,
};
use serenity::async_trait;
use songbird::driver::DecodeMode;
use songbird::events::{CoreEvent, Event, EventContext, EventHandler as VoiceHandler};
use songbird::input::{Input, RawAdapter};
use songbird::{Config as VoiceConfig, SerenityInit, Songbird};
use std::collections::HashMap;
use std::io::Cursor;
use std::sync::{Arc, Mutex};
use wallis_core::utterance::Segmenter;

#[derive(Clone, Debug)]
pub struct Settings {
    pub token: String,
    pub allowed_users: Vec<String>,
    pub read_channels: Vec<String>,
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
pub struct Out { http: Arc<serenity::http::Http>, songbird: Arc<Songbird>, runtime: tokio::runtime::Handle }

impl Out {
    pub fn message(&self, channel: u64, text: &str) -> Result<(), String> {
        let http = self.http.clone();
        let builder = CreateMessage::new().content(text).allowed_mentions(serenity::all::CreateAllowedMentions::new());
        self.runtime.block_on(ChannelId::new(channel).send_message(&http, builder)).map(|_| ()).map_err(|error| error.to_string())
    }

    pub fn file(&self, channel: u64, filename: &str, content: &str, text: &str) -> Result<(), String> {
        let http = self.http.clone();
        let builder = CreateMessage::new().content(text).add_file(CreateAttachment::bytes(content.as_bytes().to_vec(), filename)).allowed_mentions(serenity::all::CreateAllowedMentions::new());
        self.runtime.block_on(ChannelId::new(channel).send_message(&http, builder)).map(|_| ()).map_err(|error| error.to_string())
    }

    /// Queues 48 kHz stereo PCM on the guild's call; returns once playback has been handed off.
    pub fn play(&self, guild: u64, pcm: &[i16]) -> Result<(), String> {
        let call = self.songbird.get(GuildId::new(guild)).ok_or("Wallis is not in a voice channel.")?;
        let bytes: Vec<u8> = pcm.iter().flat_map(|&s| (s as f32 / 32768.0).to_le_bytes()).collect();
        let input: Input = RawAdapter::new(Cursor::new(bytes), 48_000, 2).into();
        self.runtime.block_on(async { call.lock().await.enqueue_input(input).await });
        Ok(())
    }

    /// Drops everything queued (used when the user interrupts).
    pub fn stop_speaking(&self, guild: u64) {
        if let Some(call) = self.songbird.get(GuildId::new(guild)) { self.runtime.block_on(async { call.lock().await.queue().stop() }); }
    }
}

struct Handler { settings: Settings, send: Arc<dyn Fn(Incoming) + Send + Sync>, songbird: Arc<Songbird> }

struct Receive { guild: u64, channel: u64, settings: Settings, send: Arc<dyn Fn(Incoming) + Send + Sync>, ssrc: Mutex<HashMap<u32, u64>>, segmenter: Mutex<Segmenter>, names: Arc<dyn Fn(u64) -> String + Send + Sync> }

#[async_trait]
impl VoiceHandler for Receive {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        match ctx {
            EventContext::SpeakingStateUpdate(speaking) => {
                if let Some(user) = speaking.user_id { self.ssrc.lock().expect("ssrc map poisoned").insert(speaking.ssrc, user.0); }
            }
            EventContext::VoiceTick(tick) => {
                let heard: Vec<(u32, &[i16])> = tick.speaking.iter().filter_map(|(ssrc, data)| data.decoded_voice.as_deref().map(|pcm| (*ssrc, pcm))).collect();
                let silent: Vec<u32> = tick.silent.iter().copied().collect();
                let finished = self.segmenter.lock().expect("segmenter poisoned").tick(&heard, &silent);
                for utterance in finished {
                    let Some(user) = self.ssrc.lock().expect("ssrc map poisoned").get(&utterance.speaker).copied() else { continue };
                    if !allowed(&self.settings.allowed_users, user) { continue; }
                    (self.send)(Incoming::Voice { guild: self.guild, channel: self.channel, user, speaker: (self.names)(user), pcm: utterance.pcm });
                }
            }
            _ => {}
        }
        None
    }
}

fn commands() -> Vec<CreateCommand> {
    vec![
        CreateCommand::new("join").description("Bring Wallis into your voice channel"),
        CreateCommand::new("leave").description("Send Wallis away"),
        CreateCommand::new("profile").description("Show who Wallis is"),
    ]
}

#[async_trait]
impl EventHandler for Handler {
    async fn ready(&self, ctx: Context, ready: Ready) {
        let _ = Command::set_global_commands(&ctx.http, commands()).await;
        (self.send)(Incoming::Ready { bot_id: ready.user.id.get() });
    }

    async fn message(&self, ctx: Context, message: Message) {
        let Some(guild) = message.guild_id else { return };
        if message.author.bot || message.webhook_id.is_some() || message.content.trim().is_empty() { return; }
        if !allowed(&self.settings.allowed_users, message.author.id.get()) || !self.settings.read_channels.contains(&message.channel_id.to_string()) { return; }
        let me = ctx.cache.current_user().id;
        let replied_to_me = message.referenced_message.as_ref().is_some_and(|reference| reference.author.id == me);
        let direct = message.mentions_user_id(me) || replied_to_me || wallis_core::participation::names_wallis(&message.content);
        let speaker = message.member.as_ref().and_then(|member| member.nick.clone()).unwrap_or_else(|| message.author.name.clone());
        (self.send)(Incoming::Text { guild: guild.get(), channel: message.channel_id.get(), message: message.id.get(), user: message.author.id.get(), speaker, said: message.content.clone(), direct });
    }

    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        let Interaction::Command(command) = interaction else { return };
        let reply = |text: &str, ephemeral: bool| CreateInteractionResponse::Message(CreateInteractionResponseMessage::new().content(text.to_string()).ephemeral(ephemeral));
        if !allowed(&self.settings.allowed_users, command.user.id.get()) {
            let _ = command.create_response(&ctx.http, reply("You are not on this Wallis' allow-list.", true)).await;
            return;
        }
        let Some(guild) = command.guild_id else { return };
        match command.data.name.as_str() {
            "leave" => {
                let _ = self.songbird.remove(guild).await;
                (self.send)(Incoming::Left { guild: guild.get() });
                let _ = command.create_response(&ctx.http, reply("Later.", false)).await;
            }
            "join" => {
                let channel = ctx.cache.guild(guild).and_then(|g| g.voice_states.get(&command.user.id).and_then(|state| state.channel_id));
                let Some(channel) = channel else {
                    let _ = command.create_response(&ctx.http, reply("Join a voice channel first.", true)).await;
                    return;
                };
                let _ = command.defer(&ctx.http).await;
                let outcome = self.songbird.join(guild, channel).await;
                let text = match outcome {
                    Ok(call) => {
                        let cache = ctx.cache.clone();
                        let names: Arc<dyn Fn(u64) -> String + Send + Sync> = Arc::new(move |user| cache.user(UserId::new(user)).map(|u| u.name.clone()).unwrap_or_else(|| user.to_string()));
                        let receive = Arc::new(Receive { guild: guild.get(), channel: channel.get(), settings: self.settings.clone(), send: self.send.clone(), ssrc: Mutex::default(), segmenter: Mutex::default(), names });
                        let mut call = call.lock().await;
                        call.add_global_event(CoreEvent::SpeakingStateUpdate.into(), SharedReceive(receive.clone()));
                        call.add_global_event(CoreEvent::VoiceTick.into(), SharedReceive(receive));
                        (self.send)(Incoming::Joined { guild: guild.get(), channel: channel.get() });
                        "Joined.".to_string()
                    }
                    Err(error) => format!("Could not join that channel: {error}"),
                };
                let _ = command.edit_response(&ctx.http, serenity::all::EditInteractionResponse::new().content(text)).await;
            }
            _ => {}
        }
    }
}

struct SharedReceive(Arc<Receive>);

#[async_trait]
impl VoiceHandler for SharedReceive {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> { self.0.act(ctx).await }
}

/// Connects and runs until the gateway closes. `on_event` is called from Discord's async tasks and must not block.
pub async fn run(settings: Settings, ready: impl FnOnce(Out) + Send + 'static, on_event: impl Fn(Incoming) + Send + Sync + 'static) -> Result<(), String> {
    let songbird = Songbird::serenity_from_config(VoiceConfig::default().decode_mode(DecodeMode::Decode(Default::default())));
    let intents = GatewayIntents::GUILDS | GatewayIntents::GUILD_VOICE_STATES | GatewayIntents::GUILD_MESSAGES | GatewayIntents::MESSAGE_CONTENT;
    let handler = Handler { settings: settings.clone(), send: Arc::new(on_event), songbird: songbird.clone() };
    let mut client = Client::builder(&settings.token, intents).event_handler(handler).register_songbird_with(songbird.clone()).await.map_err(|error| format!("Discord login failed: {error}"))?;
    ready(Out { http: client.http.clone(), songbird, runtime: tokio::runtime::Handle::current() });
    client.start().await.map_err(|error| format!("Discord connection ended: {error}"))
}
