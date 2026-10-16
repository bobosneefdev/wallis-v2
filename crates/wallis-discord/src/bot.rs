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
use wallis_core::{speech, worker};

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
    pub cooldown: Duration,
    pub speech_speed: f32,
    /// Folders the confined worker needs to run from (Pi, the shell), and the shell itself.
    pub sandbox_read: Vec<PathBuf>,
    pub shell: PathBuf,
}

/// Speaks and posts for one guild on behalf of the host tools.
struct Sink<'a> { out: &'a Out, guild: u64, user: u64, speed: f32 }

impl Discord for Sink<'_> {
    fn scope(&mut self, guild: u64, user: u64) { self.guild = guild; self.user = user; }
    fn queue(&mut self, path: &std::path::Path, title: &str) -> Result<(), String> { self.out.queue(self.guild, self.user, path.to_path_buf(), title.into()) }
    fn queue_list(&mut self) -> Result<String, String> { Ok(self.out.queue_list(self.guild)) }
    fn skip_audio(&mut self) -> Result<(), String> { self.out.skip_audio(self.guild) }
    fn stop_audio(&mut self) -> Result<(), String> { self.out.stop_audio(self.guild) }
    fn volume(&mut self, percent: u32) -> Result<(), String> { self.out.volume(self.guild, percent) }
    fn say(&mut self, text: &str) -> Result<(), String> { self.out.speak(self.guild, text, self.speed) }
    fn stop_speaking(&mut self) -> Result<(), String> { self.out.stop_speaking(self.guild); Ok(()) }
    fn message(&mut self, channel_id: &str, text: &str) -> Result<(), String> { self.out.message(channel_id.parse().map_err(|_| "Bad channel id")?, text) }
    fn file(&mut self, channel_id: &str, filename: &str, content: &str, text: &str) -> Result<(), String> { self.out.file(channel_id.parse().map_err(|_| "Bad channel id")?, filename, content, text) }
    fn join(&mut self, channel: Option<u64>) -> Result<u64, String> { self.out.join(self.guild, self.user, channel) }
    fn leave(&mut self) -> Result<(), String> { self.out.leave(self.guild) }
    fn voice_channel(&self) -> Option<u64> { self.out.voice_channel(self.guild) }
}

/// What the realtime agent is told for one event, led by the local time (`at`, unix seconds) so he always has a clock.
pub fn turn_prompt(at: i64, mode: Mode, channel: u64, speaker: &str, said: &str) -> String {
    let time = schedule::stamp(at);
    match mode {
        Mode::Voice => format!("[{time}] [voice transcript] {speaker}: {said}"),
        Mode::Text => format!("[{time}] [text channel {channel}] {speaker}: {said}"),
    }
}

/// The realtime Pi: memory (Mnemosyne over MCP) plus Wallis' own tools, no shell or files.
pub fn spawn_realtime(config: &BotConfig) -> Result<Agent, String> { spawn_realtime_with(config, &format!("{}\nAllowed text output channel IDs: {}. Voice requests use output channel {}. Use those IDs for delegate, message and file.",prompts::realtime_prompt(&config.persona),config.write_channels.join(", "),config.voice_text_channel)) }

/// The same Pi, but with a caller-supplied system prompt (used by the one-off profile generator).
pub fn spawn_realtime_with(config: &BotConfig, system_prompt: &str) -> Result<Agent, String> {
    let dir = config.data_dir.join("pi").join("realtime");
    std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    let mcp = prompts::realtime_mcp(&config.python, &config.data_dir.join("memory").to_string_lossy());
    std::fs::write(dir.join("mcp.json"), mcp.to_string()).map_err(|error| error.to_string())?;
    pi_config::write_settings(&dir, &config.realtime.packages, serde_json::json!({}))?;
    let cwd = config.data_dir.join("realtime-cwd");
    std::fs::create_dir_all(&cwd).map_err(|error| error.to_string())?;
    Agent::spawn(&config.pi_program, Role::Realtime, &config.realtime, &dir, system_prompt, &cwd, None)
}

struct Brain { config: BotConfig, out: Out, jobs: Jobs, reminders: Reminders, stats: Arc<Stats>, agent: Option<Agent>, last_reply_at: Option<Instant>, last_reply_user: Option<u64>, next_consider_at: Instant, started: Instant }

/// A new voice transcript is delivered by whether the agent is busy: `steer` into the running turn, or a fresh `prompt`.
/// `rx` is read between Pi's events while it works; events that are not speech wait in `later` until the turn ends.
struct Queue<'a> { rx: &'a Receiver<Incoming>, later: Vec<Incoming> }

impl Brain {
    fn ms(&self, at: Instant) -> u64 { at.duration_since(self.started).as_millis() as u64 }

    fn agent(&mut self) -> Result<&mut Agent, String> {
        if self.agent.is_none() { self.agent = Some(spawn_realtime(&self.config)?); }
        Ok(self.agent.as_mut().expect("agent just created"))
    }

    fn consider(&mut self, mode: Mode, user: u64, direct: bool) -> bool {
        let now = Instant::now();
        let participation = if mode == Mode::Voice { self.config.voice_participation } else { self.config.text_participation };
        let turn = Turn { mode, user_id: &user.to_string(), direct, participation, speaking: false };
        let last_user = self.last_reply_user.map(|id| id.to_string());
        let state = State { pending: 0, next_consider_at_ms: self.ms(self.next_consider_at), last_reply_at_ms: self.last_reply_at.map(|at| self.ms(at)).unwrap_or(0), last_reply_user: last_user.as_deref() };
        should_consider(&turn, &state, self.ms(now))
    }

    fn respond(&mut self, guild: u64, user: u64, prompt: String, queue: &mut Queue) {
        // Take the agent out for the turn so the host can borrow the job store at the same time.
        let mut agent = match self.agent() { Ok(_) => self.agent.take().expect("agent present"), Err(error) => return eprintln!("wallis: realtime agent unavailable: {error}") };
        let (out, speed, write_channels) = (self.out.clone(), self.config.speech_speed, self.config.write_channels.clone());
        let mut sink = Sink { out: &out, guild, user, speed };
        let mut host = Host { discord: &mut sink, jobs: &self.jobs, reminders: &self.reminders, guild, user, write_channels, skipped: false, profile: self.config.data_dir.join("profile.json") };
        let (stats, started) = (self.stats.clone(), Instant::now());
        let mut steer = || {
            let mut steers = Vec::new();
            for event in queue.rx.try_iter().collect::<Vec<_>>() {
                match event {
                    Incoming::Voice { speaker, pcm, .. } => {
                        // Keep listening without stopping playback; Wallis decides whether to change course.
                        match speech::transcribe(&pcm) {
                            Ok(said) if !said.is_empty() => {
                                let text = turn_prompt(schedule::now(), Mode::Voice, 0, &speaker, &said);
                                stats.steered(); stats.audit("heard", &format!("{speaker}: {said}")); stats.audit("steer", &text); steers.push(text);
                            }
                            Ok(_) => {}
                            Err(error) => eprintln!("wallis: {error}"),
                        }
                    }
                    other => queue.later.push(other),
                }
            }
            steers
        };
        self.stats.audit("prompt", &prompt);
        let audit = self.stats.clone();
        let result = agent.turn_steered(&prompt, Duration::from_secs(60), &mut steer, &mut |call| {
            if call.tool == "delegate" { audit.audit("delegate", call.args.get("task").and_then(serde_json::Value::as_str).unwrap_or("")); }
            host.handle(call)
        }, &mut |_| {});
        let spoke = !host.skipped;
        drop(host);
        stats.turn(started.elapsed().as_millis() as u64, result.is_ok());
        match result {
            Ok(_) => { self.agent = Some(agent); }
            Err(error) => { eprintln!("wallis: realtime turn failed: {error}"); }
        }
        if spoke {
            let now = Instant::now();
            self.last_reply_at = Some(now);
            self.last_reply_user = Some(user);
            self.next_consider_at = now + self.config.cooldown;
        }
    }

    /// Wakes Wallis for every reminder that has come due: each is an ordinary turn whose prompt is his own note.
    fn fire_due(&mut self, queue: &mut Queue) {
        let now = schedule::now();
        let due = match self.reminders.take_due(now) { Ok(due) => due, Err(error) => return eprintln!("wallis: {error}") };
        for reminder in due {
            self.stats.audit("reminder", &reminder.note);
            let prompt = reminder.prompt(now, self.out.voice_channel(reminder.guild), &self.config.write_channels);
            self.respond(reminder.guild, reminder.user, prompt, queue);
        }
    }

    fn handle(&mut self, event: Incoming, queue: &mut Queue) {
        match event {
            Incoming::Text { guild, channel, user, speaker, said, direct, .. } => {
                self.stats.audit("typed", &format!("{speaker}: {said}"));
                if self.consider(Mode::Text, user, direct) { self.respond(guild, user, turn_prompt(schedule::now(), Mode::Text, channel, &speaker, &said), queue); }
            }
            Incoming::Voice { guild, channel, user, speaker, pcm } => {
                let said = match speech::transcribe(&pcm) { Ok(text) if !text.is_empty() => text, Ok(_) => return, Err(error) => return eprintln!("wallis: {error}") };
                self.stats.audit("heard", &format!("{speaker}: {said}"));
                if self.consider(Mode::Voice, user, sounds_like_wallis(&said)) { self.respond(guild, user, turn_prompt(schedule::now(), Mode::Voice, channel, &speaker, &said), queue); }
            }
            Incoming::Mod { guild, channel, user, name, options, token } => {
                let (out, config) = (self.out.clone(), self.config.clone());
                // Downloads must not block live conversation or other interactions.
                std::thread::spawn(move || {
                    let result = (|| {
                        let jobs = Jobs::open(&config.data_dir.join("jobs.sqlite").to_string_lossy())?;
                        let reminders = Reminders::open(&config.data_dir.join("jobs.sqlite").to_string_lossy())?;
                        let mut sink = Sink { out: &out, guild, user, speed: config.speech_speed };
                        let mut host = Host { discord: &mut sink, jobs: &jobs, reminders: &reminders, guild, user, write_channels: vec![channel.to_string()], skipped: false, profile: config.data_dir.join("profile.json") };
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
    let interrupted = Jobs::open(&db)?.fail_interrupted()?;
    for job in interrupted { let _ = out.message(job.channel_id.parse().unwrap_or(0), &format!("Job {} was interrupted by a restart. Ask me to retry it.", job.id)); }

    let (sender, receiver): (Sender<Incoming>, Receiver<Incoming>) = channel();
    let (brain_config, brain_out, brain_db) = (config.clone(), out.clone(), db.clone());
    let stats = Arc::new(Stats::default());
    let brain_stats = stats.clone();
    std::thread::spawn(move || {
        let (Ok(jobs), Ok(reminders)) = (Jobs::open(&brain_db), Reminders::open(&brain_db)) else { return };
        let now = Instant::now();
        let mut brain = Brain { config: brain_config, out: brain_out, jobs, reminders, stats: brain_stats, agent: None, last_reply_at: None, last_reply_user: None, next_consider_at: now, started: now };
        let mut queue = Queue { rx: &receiver, later: Vec::new() };
        // Waking once a second is the reminder clock; events still arrive immediately.
        loop {
            match receiver.recv_timeout(Duration::from_secs(1)) {
                Ok(event) => brain.handle(event, &mut queue),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            brain.fire_due(&mut queue);
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
            let mut sink = Sink { out: &out, guild: 0, user: 0, speed: config.speech_speed };
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
    fn prompts_tell_the_agent_where_the_message_came_from() {
        let time = schedule::stamp(0);
        assert_eq!(turn_prompt(0, Mode::Voice, 1, "Jake", "hi"), format!("[{time}] [voice transcript] Jake: hi"));
        assert_eq!(turn_prompt(0, Mode::Text, 42, "Jake", "hi"), format!("[{time}] [text channel 42] Jake: hi"));
    }
}
