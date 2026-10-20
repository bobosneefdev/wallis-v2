//! The Wallis brain loop: one thread consumes Discord events, asks the realtime agent, and executes its tool calls;
//! a second thread runs delegated jobs on the sandbox-free worker agent (sandboxing wraps `worker_program`).
use crate::{Incoming, Out};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};
use wallis_core::agent::{Access, Agent};
use wallis_core::host::{Discord, Host};
use wallis_core::jobs::Jobs;
use wallis_core::schedule::{self, Reminders};
use wallis_core::participation::{should_consider, sounds_like_wallis, Mode, Participation, State, Turn};
use wallis_core::{pi_config, prompts};
use wallis_core::role::{Role, RoleConfig};
use wallis_core::stats::Stats;
use wallis_core::{speech, worker, media, tuning};

#[derive(Clone)]
pub struct BotConfig {
    pub pi_program: String,
    pub worker_program: String,
    pub data_dir: PathBuf,
    pub persona: String,
    pub realtime: RoleConfig,
    pub worker: RoleConfig,
    pub write_channels: Vec<String>,
    pub voice_text_channel: String,
    pub python: String,
    pub voice_participation: Participation,
    pub text_participation: Participation,
    /// Folders the confined worker needs to run from (Pi, the shell), and the shell itself.
    pub sandbox_read: Vec<PathBuf>,
    pub shell: PathBuf,
}

/// Speaks and posts for one guild on behalf of the host tools.
struct Sink<'a> { out: &'a Out, guild: u64, user: u64 }

impl Discord for Sink<'_> {
    fn scope(&mut self, guild: u64, user: u64) { self.guild = guild; self.user = user; }
    fn queue(&mut self, path: &std::path::Path, title: &str) -> Result<(), String> { self.out.queue(self.guild, self.user, path.to_path_buf(), title.into()) }
    fn queue_list(&mut self) -> Result<String, String> { Ok(self.out.queue_list(self.guild)) }
    fn skip_audio(&mut self) -> Result<(), String> { self.out.skip_audio(self.guild) }
    fn stop_audio(&mut self) -> Result<(), String> { self.out.stop_audio(self.guild) }
    fn volume(&mut self, percent: u32) -> Result<(), String> { self.out.volume(self.guild, percent) }
    fn say(&mut self, text: &str) -> Result<(), String> { self.out.speak(self.guild, text) }
    fn stop_speaking(&mut self) -> Result<(), String> { self.out.stop_speaking(self.guild); Ok(()) }
    fn message(&mut self, channel_id: &str, text: &str) -> Result<(), String> { self.out.message(channel_id.parse().map_err(|_| "Bad channel id")?, text) }
    fn file(&mut self, channel_id: &str, filename: &str, content: &str, text: &str) -> Result<(), String> { self.out.file(channel_id.parse().map_err(|_| "Bad channel id")?, filename, content, text) }
    fn history(&mut self, channel_id: &str, limit: u8) -> Result<String, String> { self.out.history(channel_id.parse().map_err(|_| "Bad channel id")?, limit) }
    fn join(&mut self, channel: Option<u64>) -> Result<u64, String> { self.out.join(self.guild, self.user, channel) }
    fn leave(&mut self) -> Result<(), String> { self.out.leave(self.guild) }
    fn voice_channel(&self) -> Option<u64> { self.out.voice_channel(self.guild) }
    fn pending_speech(&self) -> usize { self.out.pending_speech(self.guild) }
}

/// What the realtime agent is told for one event, led by the local time (`at`, unix seconds) so he always has a clock.
pub fn turn_prompt(at: i64, mode: Mode, channel: u64, speaker: &str, said: &str) -> String {
    let time = schedule::stamp(at);
    match mode {
        Mode::Voice => format!("[{time}] [voice transcript] {speaker}: {said}"),
        Mode::Text => format!("[{time}] [text channel {channel}] {speaker}: {said}"),
    }
}

/// Transcribes finished utterances, dropping silence and failures.
fn hear(events: Vec<Incoming>, stats: &Stats) -> Vec<(String, String)> {
    events.into_iter().filter_map(|event| {
        let Incoming::Voice { speaker, pcm, .. } = event else { return None };
        match speech::transcribe(&pcm) {
            Ok(said) if !said.is_empty() => { stats.audit("heard", &format!("{speaker}: {said}")); Some((speaker, said)) }
            Ok(_) => None,
            Err(error) => { eprintln!("wallis: {error}"); None }
        }
    }).collect()
}

/// Everything heard in the same moment as one message. Consecutive lines from one speaker join into a single line.
pub fn merge_voice(at: i64, heard: &[(String, String)]) -> String {
    let mut lines: Vec<(&str, String)> = Vec::new();
    for (speaker, said) in heard {
        match lines.last_mut() { Some((last, text)) if last == speaker => { text.push(' '); text.push_str(said); } _ => lines.push((speaker, said.clone())) }
    }
    let mut out = format!("[{}] [voice transcript]", schedule::stamp(at));
    for (speaker, said) in lines { out.push_str(&format!("\n{speaker}: {said}")); }
    out
}

/// The realtime Pi: memory (Mnemosyne over MCP) plus Wallis' own tools, no shell or files.
pub fn spawn_realtime(config: &BotConfig) -> Result<Agent, String> { spawn_realtime_with(config, true, &format!("{}\nAllowed text output channel IDs: {}. Voice requests use output channel {}. Use those IDs for delegate, message and file.",prompts::realtime_prompt(&config.persona),config.write_channels.join(", "),config.voice_text_channel)) }

/// pi-persona's soul and trait files. Only the realtime Pi reads them; the one-off profile generator runs without the package.
fn write_persona(dir: &std::path::Path, persona: &str) -> Result<(), String> {
    std::fs::write(dir.join("SOUL.md"), persona).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("IDENTIFY.md"), "---\nname: Wallis\nemoji: 🎧\ndescription: A person hanging out in a Discord call and chat, not an assistant.\nopenness: 0.7\nconscientiousness: 0.5\nextraversion: 0.7\nagreeableness: 0.5\nneuroticism: 0.3\nformality: 0.2\nsarcasm: 0.5\n---\n").map_err(|e| e.to_string())
}

/// The same Pi, but with a caller-supplied system prompt (used by the one-off profile generator).
pub fn spawn_realtime_with(config: &BotConfig, persona: bool, system_prompt: &str) -> Result<Agent, String> {
    let dir = config.data_dir.join("pi").join("realtime");
    std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    let mcp = prompts::realtime_mcp(&config.python, &config.data_dir.join("memory").to_string_lossy());
    std::fs::write(dir.join("mcp.json"), mcp.to_string()).map_err(|error| error.to_string())?;
    if persona { write_persona(&dir, &config.persona)?; }
    let packages = if persona { pi_config::with_persona(&config.realtime.packages) } else { config.realtime.packages.clone() };
    pi_config::write_settings(&dir, &packages, serde_json::json!({}))?;
    let cwd = config.data_dir.join("realtime-cwd");
    std::fs::create_dir_all(&cwd).map_err(|error| error.to_string())?;
    Agent::spawn(&config.pi_program, Role::Realtime, &config.realtime, &dir, system_prompt, &cwd, None)
}

fn names(members: &[(u64, String)]) -> String { if members.is_empty() { "nobody else".into() } else { members.iter().map(|(_, name)| name.as_str()).collect::<Vec<_>>().join(", ") } }

/// The quiet note for a join, leave or move touching his call `mine`; None when it does not concern him.
fn presence_note(mine: u64, speaker: &str, from: u64, to: u64) -> Option<String> {
    if from == to { None } else if mine == to { Some(format!("{speaker} just joined the call; they did not hear anything said before.")) } else if mine == from { Some(format!("{speaker} left the call.")) } else { None }
}

/// Added to a voice turn: the full roster the first time he hears this call, then only what changed. Presence is who could hear something, never proof they heard earlier talk.
fn voice_context(channel: u64, members: &[(u64, String)], first: bool, notes: &[String]) -> String {
    if first { format!("\n[call] You are in voice channel {channel} with {}. Being in the call now does not mean someone heard what was said before they arrived.", names(members)) }
    else { notes.iter().map(|note| format!("\n[call] {note}")).collect() }
}

/// Added to a text turn while he is in a call: whether the author is in it, and that the reply belongs in the text channel.
fn text_context(voice: u64, members: &[(u64, String)], user: u64, channel: u64) -> String {
    let heard = if members.iter().any(|(id, _)| *id == user) { "They are in it, but being in it now does not mean they heard earlier call talk" } else { "They are NOT in it and heard none of the call" };
    format!("\n[call] You are also in voice channel {voice} with {}. The author of this text message: {heard}. Reply in this text channel with `message` (channelId {channel}) unless they ask you to speak. Make it make sense from this channel's own history: explain a call-only reference briefly and leave unrelated call talk out.", names(members))
}

/// Only speech from the call he is answering in (same guild and voice channel) may steer a running voice turn.
fn steers(event: &Incoming, guild: u64, call: Option<u64>) -> bool { matches!(event, Incoming::Voice { guild: g, channel, .. } if *g == guild && Some(*channel) == call) }

/// When Wallis last spoke up in one conversation (a voice or text channel id) and when he may next. Conversations never share a cooldown.
#[derive(Default)]
struct Convo { last_reply_at: Option<Instant>, last_reply_user: Option<u64>, next_consider_at: Option<Instant> }

struct Brain { config: BotConfig, out: Out, jobs: Jobs, reminders: Reminders, stats: Arc<Stats>, agent: Option<Agent>, convos: std::collections::HashMap<u64, Convo>, started: Instant, media: media::History, last_text_reply: Option<(u64, u64, Instant)>, speech_notes: std::collections::HashMap<u64, String>,
    /// Joins and leaves in his call not yet shown to him (per guild), and the voice channel whose roster he was last given.
    presence: std::collections::HashMap<u64, Vec<String>>, announced: std::collections::HashMap<u64, u64> }

/// A new voice transcript is delivered by whether the agent is busy: `steer` into the running turn, or a fresh `prompt`.
/// `rx` is read between Pi's events while it works; events that are not speech wait in `later` until the turn ends.
struct Queue<'a> { rx: &'a Receiver<Incoming>, later: Vec<Incoming> }

impl Brain {
    fn ms(&self, at: Instant) -> u64 { at.duration_since(self.started).as_millis() as u64 }

    fn agent(&mut self) -> Result<&mut Agent, String> {
        if self.agent.is_none() { self.agent = Some(spawn_realtime(&self.config)?); }
        Ok(self.agent.as_mut().expect("agent just created"))
    }

    fn consider(&mut self, mode: Mode, convo: u64, user: u64, direct: bool) -> bool {
        let now = Instant::now();
        let convo = self.convos.get(&convo);
        let participation = if mode == Mode::Voice { self.config.voice_participation } else { self.config.text_participation };
        let turn = Turn { mode, user_id: &user.to_string(), direct, participation, speaking: false };
        let last_user = convo.and_then(|c| c.last_reply_user).map(|id| id.to_string());
        let state = State { follow_up_ms: tuning::current().follow_up_secs as u64 * 1000, pending: 0, next_consider_at_ms: convo.and_then(|c| c.next_consider_at).map(|at| self.ms(at)).unwrap_or(0), last_reply_at_ms: convo.and_then(|c| c.last_reply_at).map(|at| self.ms(at)).unwrap_or(0), last_reply_user: last_user.as_deref() };
        should_consider(&turn, &state, self.ms(now))
    }

    /// Everyone in his call right now (the voice channel and its members), if he is in one. Leaving a call forgets what he was told about it.
    fn roster(&mut self, guild: u64) -> Option<(u64, Vec<(u64, String)>)> {
        let Some(channel) = self.out.voice_channel(guild) else { self.announced.remove(&guild); self.presence.remove(&guild); return None };
        Some((channel, self.out.voice_members(channel)))
    }

    fn voice_prompt_context(&mut self, guild: u64) -> String {
        let Some((channel, members)) = self.roster(guild) else { return String::new() };
        let notes = self.presence.remove(&guild).unwrap_or_default();
        let first = self.announced.insert(guild, channel) != Some(channel);
        voice_context(channel, &members, first, &notes)
    }

    fn text_prompt_context(&mut self, guild: u64, user: u64, channel: u64) -> String {
        self.roster(guild).map(|(voice, members)| text_context(voice, &members, user, channel)).unwrap_or_default()
    }

    /// `origin`: the text channel this turn answers (0 in the voice call). It decides who may steer the turn and where delayed work lands.
    fn respond(&mut self, guild: u64, user: u64, mut prompt: String, images: &[media::Image], origin: u64, queue: &mut Queue) {
        // Only the call he is answering in may steer a voice turn; a text turn is never steered by what is said aloud.
        let steer_channel = if origin == 0 { self.out.voice_channel(guild) } else { None };
        let convo = if origin != 0 { origin } else { steer_channel.unwrap_or(0) };
        if let Some(note) = self.speech_notes.remove(&guild) { prompt.push_str(&format!("\n[speech recovery] {note}")); }
        // Take the agent out for the turn so the host can borrow the job store at the same time.
        let mut agent = match self.agent() { Ok(_) => self.agent.take().expect("agent present"), Err(error) => return eprintln!("wallis: realtime agent unavailable: {error}") };
        let (out, write_channels) = (self.out.clone(), self.config.write_channels.clone());
        let mut sink = Sink { out: &out, guild, user };
        let mut host = Host { discord: &mut sink, jobs: &self.jobs, reminders: &self.reminders, guild, user, write_channels, skipped: false, origin, profile: self.config.data_dir.join("profile.json") };
        let (stats, started) = (self.stats.clone(), Instant::now());
        // Several people talk at once in a fast room: transcribe everything that arrived and hand it over as ONE message, with
        // how far behind his speech is, so he can drop what is stale instead of working through each line in order.
        let mut steer = || {
            let (voices, others): (Vec<_>, Vec<_>) = queue.rx.try_iter().partition(|event| steers(event, guild, steer_channel));
            queue.later.extend(others);
            let heard = hear(voices, &stats);
            if heard.is_empty() { return vec![]; }
            let behind = out.pending_speech(guild);
            let note = if behind >= 2 { format!("\n[you still have {behind} sentences queued to speak; if they are stale, call stop_speaking and catch up in one short line]") } else { String::new() };
            let text = format!("{}{note}", merge_voice(schedule::now(), &heard));
            stats.steered(); stats.audit("steer", &text);
            vec![text]
        };
        self.stats.audit("prompt", &prompt);
        let audit = self.stats.clone();
        let left = std::cell::Cell::new(false);
        let result = agent.turn_with_images(&prompt, images, Duration::from_secs(60), &mut steer, &mut |call| {
            if call.tool == "delegate" { audit.audit("delegate", call.args.get("task").and_then(serde_json::Value::as_str).unwrap_or("")); }
            if call.tool == "leave_call" { left.set(true); }
            host.handle(call)
        }, &mut |_| {});
        let spoke = !host.skipped;
        drop(host);
        // Leaving and later rejoining, even the same channel, is a new call: he is told the roster again.
        if left.get() { self.announced.remove(&guild); self.presence.remove(&guild); }
        stats.turn(started.elapsed().as_millis() as u64, result.is_ok());
        match result {
            Ok(_) => { self.agent = Some(agent); }
            Err(error) => { eprintln!("wallis: realtime turn failed: {error}"); }
        }
        if spoke {
            let now = Instant::now();
            self.convos.insert(convo, Convo { last_reply_at: Some(now), last_reply_user: Some(user), next_consider_at: Some(now + Duration::from_secs(tuning::current().cooldown_secs.into())) });
        }
    }

    /// Wakes Wallis for every reminder that has come due: each is an ordinary turn whose prompt is his own note.
    fn fire_due(&mut self, queue: &mut Queue) {
        let now = schedule::now();
        let due = match self.reminders.take_due(now) { Ok(due) => due, Err(error) => return eprintln!("wallis: {error}") };
        for reminder in due {
            self.stats.audit("reminder", &reminder.note);
            let prompt = reminder.prompt(now, self.out.voice_channel(reminder.guild), &self.config.write_channels);
            self.respond(reminder.guild, reminder.user, prompt, &[], reminder.origin, queue);
        }
    }

    /// Hands every finished worker job to Wallis privately: an ordinary turn whose prompt carries the full report. Nothing is posted to the channel.
    fn deliver_jobs(&mut self, queue: &mut Queue) {
        let jobs = match self.jobs.take_finished() { Ok(jobs) => jobs, Err(error) => return eprintln!("wallis: {error}") };
        for job in jobs {
            self.stats.audit("job", &format!("{} {}", job.id, job.status));
            let prompt = job.prompt(schedule::now(), self.out.voice_channel(job.guild), &self.config.write_channels);
            self.respond(job.guild, job.user, prompt, &[], job.origin, queue);
        }
    }

    fn handle(&mut self, event: Incoming, queue: &mut Queue) {
        match event {
            Incoming::SpeechFailed { guild, text, tuning: failed, generation, retried, error } => {
                // Ignore failures from speech that was deliberately stopped, including stale queued errors.
                if self.out.speech_generation(guild) != generation { return; }
                eprintln!("wallis: speech: {error}");
                self.stats.audit("speech_error", &error);
                self.out.stop_speaking(guild);
                if !retried {
                    let saved = tuning::recover(&self.config.data_dir, &failed);
                    let note = format!("Speech failed ({error}). Safe speech settings were restored unless you already changed them. The failed sentence is being retried once. Do not repeat the failed tuning.{}", saved.err().map(|e| format!(" Recovery is live but could not be saved: {e}")).unwrap_or_default());
                    self.speech_notes.insert(guild, note);
                    if self.out.speak_retry(guild, &text, true).is_ok() { return; }
                }
                let channel = std::iter::once(&self.config.voice_text_channel).chain(&self.config.write_channels).find(|id| self.config.write_channels.contains(id) && id.parse::<u64>().is_ok()).and_then(|id| id.parse::<u64>().ok());
                let delivered = if let Some(channel) = channel {
                    let message: Vec<char> = format!("Voice playback is unavailable. Here is what I was saying:\n{text}").chars().collect();
                    message.chunks(1900).try_for_each(|chunk| self.out.message(channel, &chunk.iter().collect::<String>()))
                } else { Err("No allowed text output channel is configured.".into()) };
                let delivery = match delivered { Ok(()) => "The failed reply was sent to an allowed text channel.".to_string(), Err(error) => { eprintln!("wallis: speech text fallback: {error}"); format!("Text fallback also failed: {error}") } };
                self.speech_notes.insert(guild, format!("Speech failed even after recovery. {delivery} Use message for now; do not keep trying say or the failed tuning."));
            }
            Incoming::Text { guild, channel, message, user, speaker, said, direct, mut media, reply } => {
                self.stats.audit("typed", &format!("{speaker}: {said}"));
                self.media.remember(channel, message, user, &media);
                // An upload immediately after Wallis answered is a follow-up, including in mentions-only mode.
                let follow_up = !media.is_empty() && self.last_text_reply.is_some_and(|(last_channel, last_user, at)| last_channel == channel && last_user == user && at.elapsed() < Duration::from_secs(120));
                if !self.consider(Mode::Text, channel, user, direct || follow_up) { return; }
                let media_started = Instant::now();
                if said.contains("https://") {
                    let refreshed = self.out.media(channel, message);
                    if !refreshed.is_empty() { media = refreshed; self.media.remember(channel, message, user, &media); }
                }
                let (source_message, sources) = if media.is_empty() {
                    self.media.resolve(channel, user, reply, &said).unwrap_or_else(|| {
                        reply.map(|id| (id, self.out.media(channel, id))).unwrap_or((message, vec![]))
                    })
                } else { (message, media) };
                let mut prompt = turn_prompt(schedule::now(), Mode::Text, channel, &speaker, &said);
                let mut images = Vec::new();
                let frame_limit = (12 / sources.len().max(1)).min(6);
                for (index, source) in sources.iter().enumerate() {
                    let remaining = Duration::from_secs(20).saturating_sub(media_started.elapsed());
                    if remaining.is_zero() { prompt.push_str("\n[remaining media unavailable: the turn's 20 second media budget was exhausted]"); break; }
                    let first = images.len() + 1;
                    match media::prepare(&self.config.python, source, frame_limit, remaining) {
                        Ok(prepared) => {
                            let status = if prepared.images.is_empty() { "unavailable".to_string() } else { format!("image inputs {first}–{}", first + prepared.images.len() - 1) };
                            prompt.push_str(&format!("\n[media {} from message {source_message}; {status}] {}", index + 1, prepared.note));
                            images.extend(prepared.images);
                        }
                        Err(error) => prompt.push_str(&format!("\n[media {} from message {source_message} unavailable] {error}", index + 1)),
                    }
                }
                prompt.push_str(&self.text_prompt_context(guild, user, channel));
                let before = self.convos.get(&channel).and_then(|c| c.last_reply_at);
                self.respond(guild, user, prompt, &images, channel, queue);
                let after = self.convos.get(&channel).and_then(|c| c.last_reply_at);
                if after != before { self.last_text_reply = after.map(|at| (channel, user, at)); }
            }
            Incoming::MediaUpdate { channel, message, user, media } => self.media.remember(channel, message, user, &media),
            Incoming::MediaDelete { channel, messages } => self.media.forget(channel, &messages),
            Incoming::Voice { guild, channel, user, speaker, pcm } => {
                // Utterances that piled up while the last turn ran are heard together, not answered one by one.
                let (voices, others): (Vec<_>, Vec<_>) = queue.rx.try_iter().partition(|event| steers(event, guild, Some(channel)));
                queue.later.extend(others);
                let mut heard = hear(std::iter::once(Incoming::Voice { guild, channel, user, speaker, pcm }).chain(voices).collect(), &self.stats);
                if heard.is_empty() { return; }
                let direct = heard.iter().any(|(_, said)| sounds_like_wallis(said));
                if self.consider(Mode::Voice, channel, user, direct) {
                    let prompt = if heard.len() == 1 { let (speaker, said) = heard.remove(0); turn_prompt(schedule::now(), Mode::Voice, channel, &speaker, &said) } else { merge_voice(schedule::now(), &heard) };
                    let prompt = format!("{prompt}{}", self.voice_prompt_context(guild));
                    self.respond(guild, user, prompt, &[], 0, queue);
                }
            }
            Incoming::Presence { guild, speaker, from, to, .. } => {
                // A quiet note for his next voice turn: never a greeting, never an interruption.
                let Some(note) = self.out.voice_channel(guild).and_then(|mine| presence_note(mine, &speaker, from, to)) else { return };
                let notes = self.presence.entry(guild).or_default();
                notes.push(note);
                if notes.len() > 8 { notes.remove(0); }
            }
            Incoming::Mod { guild, channel, user, name, options, token } => {
                let (out, config) = (self.out.clone(), self.config.clone());
                // Downloads must not block live conversation or other interactions.
                std::thread::spawn(move || {
                    let result = (|| {
                        let jobs = Jobs::open(&config.data_dir.join("jobs.sqlite").to_string_lossy())?;
                        let reminders = Reminders::open(&config.data_dir.join("jobs.sqlite").to_string_lossy())?;
                        let mut sink = Sink { out: &out, guild, user };
                        let mut host = Host { discord: &mut sink, jobs: &jobs, reminders: &reminders, guild, user, write_channels: vec![channel.to_string()], skipped: false, origin: channel, profile: config.data_dir.join("profile.json") };
                        wallis_core::mods::run(&config.data_dir, &name, serde_json::json!({"options":options,"guild":guild.to_string(),"user":user.to_string(),"channel":channel.to_string()}), &mut |call| host.handle(call))
                    })();
                    if let Err(error) = out.reply_mod(&token, &result.unwrap_or_else(|e| format!("Failed: {e}"))) { eprintln!("wallis: {error}"); }
                });
            }
            Incoming::Ready { .. } => {}
        }
    }
}

/// The running brain and worker threads. Dropping it stops them (the worker after its current job).
pub struct Bot { pub sender: Sender<Incoming>, pub stats: Arc<Stats>, stop: Arc<AtomicBool> }

impl Drop for Bot {
    fn drop(&mut self) { self.stop.store(true, Ordering::Relaxed); }
}

/// Starts the brain and worker threads. Feed Discord events into `Bot::sender`.
pub fn start(config: BotConfig, out: Out) -> Result<Bot, String> {
    std::fs::create_dir_all(&config.data_dir).map_err(|error| error.to_string())?;
    let db = config.data_dir.join("jobs.sqlite").to_string_lossy().to_string();
    // Interrupted jobs are marked failed and reach Wallis like any other finished job, so he tells the user.
    Jobs::open(&db)?.fail_interrupted()?;

    let (sender, receiver): (Sender<Incoming>, Receiver<Incoming>) = channel();
    let (brain_config, brain_out, brain_db) = (config.clone(), out.clone(), db.clone());
    let stats = Arc::new(Stats::default());
    let brain_stats = stats.clone();
    std::thread::spawn(move || {
        let (Ok(jobs), Ok(reminders)) = (Jobs::open(&brain_db), Reminders::open(&brain_db)) else { return };
        let now = Instant::now();
        let mut brain = Brain { config: brain_config, out: brain_out, jobs, reminders, stats: brain_stats, agent: None, convos: Default::default(), started: now, media: media::History::default(), last_text_reply: None, speech_notes: Default::default(), presence: Default::default(), announced: Default::default() };
        let mut queue = Queue { rx: &receiver, later: Vec::new() };
        // Waking once a second is the reminder clock; events still arrive immediately.
        loop {
            match receiver.recv_timeout(Duration::from_secs(1)) {
                Ok(event) => brain.handle(event, &mut queue),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            brain.fire_due(&mut queue);
            brain.deliver_jobs(&mut queue);
            while !queue.later.is_empty() { let next = queue.later.remove(0); brain.handle(next, &mut queue); }
        }
    });

    let stop = Arc::new(AtomicBool::new(false));
    let worker_stop = stop.clone();
    std::thread::spawn(move || {
        let Ok(jobs) = Jobs::open(&db) else { return };
        let scratch = config.data_dir.join("worker-scratch");
        let dir = config.data_dir.join("pi").join("worker");
        while !worker_stop.load(Ordering::Relaxed) {
            let mut sink = Sink { out: &out, guild: 0, user: 0 };
            let mut start = || {

                let shell_bin = config.shell.parent().map(PathBuf::from).unwrap_or_default();
                let access = Access { read: config.sandbox_read.clone(), write: vec![scratch.clone(), dir.clone(), config.data_dir.join("mods"), config.data_dir.join("media")], path: vec![shell_bin] };
                Agent::spawn(&config.worker_program, Role::Worker, &config.worker, &dir, &prompts::worker_prompt(), &scratch, Some(&access))
            };
            match worker::run_next(&jobs, &mut sink, &mut start, Duration::from_secs(300), &config.data_dir) {
                Ok(Some(_)) => { if let Err(error) = out.set_commands() { eprintln!("wallis: {error}"); } }
                Ok(None) => std::thread::sleep(Duration::from_secs(1)),
                Err(error) => { eprintln!("wallis: {error}"); std::thread::sleep(Duration::from_secs(5)); }
            }
        }
    });
    Ok(Bot { sender, stats, stop })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_burst_of_voice_lines_becomes_one_message() {
        let heard = |s: &str, t: &str| (s.to_string(), t.to_string());
        let merged = merge_voice(0, &[heard("Jake", "so I was"), heard("Jake", "saying"), heard("Mia", "wait what"), heard("Jake", "never mind")]);
        assert_eq!(merged, format!("[{}] [voice transcript]\nJake: so I was saying\nMia: wait what\nJake: never mind", schedule::stamp(0)));
    }

    #[test]
    fn prompts_tell_the_agent_where_the_message_came_from() {
        let time = schedule::stamp(0);
        assert_eq!(turn_prompt(0, Mode::Voice, 1, "Jake", "hi"), format!("[{time}] [voice transcript] Jake: hi"));
        assert_eq!(turn_prompt(0, Mode::Text, 42, "Jake", "hi"), format!("[{time}] [text channel 42] Jake: hi"));
    }

    fn voice(guild: u64, channel: u64) -> Incoming { Incoming::Voice { guild, channel, user: 1, speaker: "A".into(), pcm: vec![] } }

    #[test]
    fn a_text_author_outside_the_call_is_told_and_pointed_at_the_text_channel() {
        let members = vec![(1, "Jake".to_string())];
        let outside = text_context(7, &members, 2, 42);
        assert!(outside.contains("NOT in it") && outside.contains("channelId 42") && outside.contains("Jake"));
        let inside = text_context(7, &members, 1, 42);
        assert!(inside.contains("They are in it") && inside.contains("does not mean they heard") && !inside.contains("NOT in it"));
        assert!(text_context(7, &[], 2, 42).contains("nobody else"));
    }

    #[test]
    fn joins_leaves_and_moves_only_matter_for_his_call() {
        assert!(presence_note(7, "Mia", 0, 7).unwrap().contains("Mia just joined") && presence_note(7, "Mia", 7, 0).unwrap().contains("Mia left"));
        assert!(presence_note(7, "Mia", 7, 9).unwrap().contains("left") && presence_note(7, "Mia", 9, 7).unwrap().contains("joined"), "a move is a leave or a join from his side");
        assert_eq!((presence_note(7, "Mia", 8, 9), presence_note(7, "Mia", 7, 7), presence_note(7, "Mia", 0, 0)), (None, None, None));
    }

    #[test]
    fn the_roster_comes_once_then_only_the_changes() {
        let members = vec![(1, "Jake".to_string()), (2, "Mia".to_string())];
        let first = voice_context(7, &members, true, &["Zed just joined the call.".into()]);
        assert!(first.contains("with Jake, Mia") && !first.contains("Zed"), "notes made before the first roster are folded into it");
        let later = voice_context(7, &members, false, &["Zed just joined the call; they did not hear anything said before.".into()]);
        assert!(later.contains("[call] Zed just joined") && !later.contains("with Jake"));
        assert_eq!(voice_context(7, &members, false, &[]), "");
    }

    #[test]
    fn only_speech_from_the_same_call_steers() {
        assert!(steers(&voice(1, 7), 1, Some(7)));
        assert!(!steers(&voice(1, 8), 1, Some(7)), "another voice channel");
        assert!(!steers(&voice(2, 7), 1, Some(7)), "another guild");
        assert!(!steers(&voice(1, 7), 1, None), "a text turn is never steered by speech");
        assert!(!steers(&Incoming::Ready { bot_id: 1 }, 1, Some(7)));
    }

    #[test]
    fn conversations_keep_separate_cooldowns() {
        let started = Instant::now();
        let mut convos: std::collections::HashMap<u64, Convo> = Default::default();
        convos.insert(7, Convo { last_reply_at: Some(started), last_reply_user: Some(1), next_consider_at: Some(started + Duration::from_secs(60)) });
        let ms = |at: Instant| at.duration_since(started).as_millis() as u64;
        let natural = |convo: u64, now: Instant| {
            let c = convos.get(&convo);
            let state = State { follow_up_ms: 0, pending: 0, next_consider_at_ms: c.and_then(|c| c.next_consider_at).map(ms).unwrap_or(0), last_reply_at_ms: 0, last_reply_user: None };
            should_consider(&Turn { mode: Mode::Text, user_id: "1", direct: false, participation: Participation::Natural, speaking: false }, &state, ms(now))
        };
        let soon = started + Duration::from_secs(5);
        assert!(!natural(7, soon), "cooling down where he just spoke");
        assert!(natural(42, soon), "a different channel is unaffected");
    }
}
