//! The Wallis brain loop: one thread consumes Discord events, asks the realtime agent, and executes its tool calls;
//! a second thread runs delegated jobs on the sandbox-free worker agent (sandboxing wraps `worker_program`).
use crate::{Incoming, Out};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};
use wallis_core::agent::{Access, Agent};
use wallis_core::host::{Discord, Host};
use wallis_core::jobs::Jobs;
use wallis_core::participation::{names_wallis, should_consider, Mode, Participation, State, Turn};
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
struct Sink<'a> { out: &'a Out, guild: u64, speed: f32 }

impl Discord for Sink<'_> {
    fn say(&mut self, text: &str) -> Result<(), String> { self.out.speak(self.guild, text, self.speed) }
    fn message(&mut self, channel_id: &str, text: &str) -> Result<(), String> { self.out.message(channel_id.parse().map_err(|_| "Bad channel id")?, text) }
    fn file(&mut self, channel_id: &str, filename: &str, content: &str, text: &str) -> Result<(), String> { self.out.file(channel_id.parse().map_err(|_| "Bad channel id")?, filename, content, text) }
}

/// Worker output goes to the channel the job named; speaking is not available there.
struct WorkerSink<'a>(&'a Out);

impl Discord for WorkerSink<'_> {
    fn say(&mut self, _: &str) -> Result<(), String> { Err("The worker cannot speak.".into()) }
    fn message(&mut self, channel_id: &str, text: &str) -> Result<(), String> { self.0.message(channel_id.parse().map_err(|_| "Bad channel id")?, text) }
    fn file(&mut self, channel_id: &str, filename: &str, content: &str, text: &str) -> Result<(), String> { self.0.file(channel_id.parse().map_err(|_| "Bad channel id")?, filename, content, text) }
}

/// What the realtime agent is told for one event.
pub fn turn_prompt(mode: Mode, channel: u64, speaker: &str, said: &str) -> String {
    match mode {
        Mode::Voice => format!("[voice call] {speaker}: {said}"),
        Mode::Text => format!("[text channel {channel}] {speaker}: {said}"),
    }
}

/// The realtime Pi: memory (Mnemosyne over MCP) plus Wallis' own tools, no shell or files.
pub fn spawn_realtime(config: &BotConfig) -> Result<Agent, String> {
    let dir = config.data_dir.join("pi").join("realtime");
    std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    let mcp = prompts::realtime_mcp(&config.python, &config.data_dir.join("memory").to_string_lossy());
    std::fs::write(dir.join("mcp.json"), mcp.to_string()).map_err(|error| error.to_string())?;
    pi_config::write_settings(&dir, &config.realtime.packages, serde_json::json!({}))?;
    let cwd = config.data_dir.join("realtime-cwd");
    std::fs::create_dir_all(&cwd).map_err(|error| error.to_string())?;
    Agent::spawn(&config.pi_program, Role::Realtime, &config.realtime, &dir, &prompts::realtime_prompt(&config.persona), &cwd, None)
}

struct Brain { config: BotConfig, out: Out, jobs: Jobs, stats: Arc<Stats>, agent: Option<Agent>, last_reply_at: Option<Instant>, last_reply_user: Option<u64>, next_consider_at: Instant, started: Instant }

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
        let mut sink = Sink { out: &out, guild, speed };
        let mut host = Host { discord: &mut sink, jobs: &self.jobs, write_channels, skipped: false, profile: self.config.data_dir.join("profile.json") };
        let (stats, started) = (self.stats.clone(), Instant::now());
        let mut steer = || {
            let mut steers = Vec::new();
            for event in queue.rx.try_iter().collect::<Vec<_>>() {
                match event {
                    Incoming::Voice { guild, user, speaker, pcm, .. } => {
                        // Talking over Wallis cuts him off, then what they said steers the turn already underway.
                        out.stop_speaking(guild);
                        match speech::transcribe(&pcm) {
                            Ok(said) if !said.is_empty() => { stats.steered(); steers.push(turn_prompt(Mode::Voice, 0, &speaker, &said)); let _ = user; }
                            Ok(_) => {}
                            Err(error) => eprintln!("wallis: {error}"),
                        }
                    }
                    other => queue.later.push(other),
                }
            }
            steers
        };
        let result = agent.turn_steered(&prompt, Duration::from_secs(60), &mut steer, &mut |call| host.handle(call), &mut |_| {});
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

    fn handle(&mut self, event: Incoming, queue: &mut Queue) {
        match event {
            Incoming::Text { guild, channel, user, speaker, said, direct, .. } => {
                if self.consider(Mode::Text, user, direct) { self.respond(guild, user, turn_prompt(Mode::Text, channel, &speaker, &said), queue); }
            }
            Incoming::Voice { guild, channel, user, speaker, pcm } => {
                self.out.stop_speaking(guild);
                let said = match speech::transcribe(&pcm) { Ok(text) if !text.is_empty() => text, Ok(_) => return, Err(error) => return eprintln!("wallis: {error}") };
                if self.consider(Mode::Voice, user, names_wallis(&said)) { self.respond(guild, user, turn_prompt(Mode::Voice, channel, &speaker, &said), queue); }
            }
            Incoming::Joined { guild, .. } => self.respond(guild, 0, "[system] You just joined the voice call. Greet everyone with one short line.".into(), queue),
            Incoming::Left { guild } => self.out.stop_speaking(guild),
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
        let Ok(jobs) = Jobs::open(&brain_db) else { return };
        let now = Instant::now();
        let mut brain = Brain { config: brain_config, out: brain_out, jobs, stats: brain_stats, agent: None, last_reply_at: None, last_reply_user: None, next_consider_at: now, started: now };
        let mut queue = Queue { rx: &receiver, later: Vec::new() };
        while let Ok(event) = receiver.recv() {
            brain.handle(event, &mut queue);
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
            let mut sink = WorkerSink(&out);
            let mut start = || {
                std::fs::create_dir_all(&scratch).map_err(|error| error.to_string())?;
                std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
                pi_config::write_settings(&dir, &config.worker.packages, serde_json::json!({ "shellPath": config.shell }))?;
                let shell_bin = config.shell.parent().map(PathBuf::from).unwrap_or_default();
                let access = Access { read: config.sandbox_read.clone(), write: vec![scratch.clone(), dir.clone()], path: vec![shell_bin] };
                Agent::spawn(&config.worker_program, Role::Worker, &config.worker, &dir, &prompts::worker_prompt(), &scratch, Some(&access))
            };
            match worker::run_next(&jobs, &mut sink, &mut start, Duration::from_secs(300)) {
                Ok(Some(_)) => {}
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
        assert_eq!(turn_prompt(Mode::Voice, 1, "Jake", "hi"), "[voice call] Jake: hi");
        assert_eq!(turn_prompt(Mode::Text, 42, "Jake", "hi"), "[text channel 42] Jake: hi");
    }
}
