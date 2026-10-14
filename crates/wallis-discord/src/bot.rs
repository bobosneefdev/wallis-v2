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
use wallis_core::prompts;
use wallis_core::role::{Role, RoleConfig};
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
    fn say(&mut self, text: &str) -> Result<(), String> { self.out.play(self.guild, &speech::synthesize(text, self.speed)?) }
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
    let cwd = config.data_dir.join("realtime-cwd");
    std::fs::create_dir_all(&cwd).map_err(|error| error.to_string())?;
    Agent::spawn(&config.pi_program, Role::Realtime, &config.realtime, &dir, &prompts::realtime_prompt(&config.persona), &cwd, None)
}

struct Brain { config: BotConfig, out: Out, jobs: Jobs, agent: Option<Agent>, last_reply_at: Option<Instant>, last_reply_user: Option<u64>, next_consider_at: Instant, started: Instant }

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

    fn respond(&mut self, mode: Mode, guild: u64, channel: u64, user: u64, prompt: String) {
        // Take the agent out for the turn so the host can borrow the job store at the same time.
        let mut agent = match self.agent() { Ok(_) => self.agent.take().expect("agent present"), Err(error) => return eprintln!("wallis: realtime agent unavailable: {error}") };
        let (out, speed, write_channels) = (self.out.clone(), self.config.speech_speed, self.config.write_channels.clone());
        let mut sink = Sink { out: &out, guild, speed };
        let mut host = Host { discord: &mut sink, jobs: &self.jobs, write_channels, skipped: false };
        let result = agent.turn(&prompt, Duration::from_secs(60), &mut |call| host.handle(call), &mut |_| {});
        let spoke = !host.skipped;
        drop(host);
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
        let _ = (mode, channel);
    }

    fn handle(&mut self, event: Incoming) {
        match event {
            Incoming::Text { guild, channel, user, speaker, said, direct, .. } => {
                if self.consider(Mode::Text, user, direct) { self.respond(Mode::Text, guild, channel, user, turn_prompt(Mode::Text, channel, &speaker, &said)); }
            }
            Incoming::Voice { guild, channel, user, speaker, pcm } => {
                // Talking over Wallis cuts him off.
                self.out.stop_speaking(guild);
                let said = match speech::transcribe(&pcm) { Ok(text) if !text.is_empty() => text, Ok(_) => return, Err(error) => return eprintln!("wallis: {error}") };
                let direct = names_wallis(&said);
                if self.consider(Mode::Voice, user, direct) { self.respond(Mode::Voice, guild, channel, user, turn_prompt(Mode::Voice, channel, &speaker, &said)); }
            }
            Incoming::Joined { guild, .. } => self.respond(Mode::Voice, guild, 0, 0, "[system] You just joined the voice call. Greet everyone with one short line.".into()),
            Incoming::Left { guild } => self.out.stop_speaking(guild),
            Incoming::Ready { .. } => {}
        }
    }
}

/// The running brain and worker threads. Dropping it stops them (the worker after its current job).
pub struct Bot { pub sender: Sender<Incoming>, stop: Arc<AtomicBool> }

impl Drop for Bot {
    fn drop(&mut self) { self.stop.store(true, Ordering::Relaxed); }
}

/// Merges back-to-back voice utterances from one speaker (a thought spoken in two breaths) into a single turn.
pub fn coalesce(events: Vec<Incoming>) -> Vec<Incoming> {
    let mut merged: Vec<Incoming> = Vec::new();
    for event in events {
        if let (Some(Incoming::Voice { guild: g, user: u, pcm: previous, .. }), Incoming::Voice { guild, user, pcm, .. }) = (merged.last_mut(), &event) {
            if g == guild && u == user { previous.extend_from_slice(pcm); continue; }
        }
        merged.push(event);
    }
    merged
}

/// Starts the brain and worker threads. Feed Discord events into `Bot::sender`.
pub fn start(config: BotConfig, out: Out) -> Result<Bot, String> {
    std::fs::create_dir_all(&config.data_dir).map_err(|error| error.to_string())?;
    let db = config.data_dir.join("jobs.sqlite").to_string_lossy().to_string();
    let interrupted = Jobs::open(&db)?.fail_interrupted()?;
    for job in interrupted { let _ = out.message(job.channel_id.parse().unwrap_or(0), &format!("Job {} was interrupted by a restart. Ask me to retry it.", job.id)); }

    let (sender, receiver): (Sender<Incoming>, Receiver<Incoming>) = channel();
    let (brain_config, brain_out, brain_db) = (config.clone(), out.clone(), db.clone());
    std::thread::spawn(move || {
        let Ok(jobs) = Jobs::open(&brain_db) else { return };
        let now = Instant::now();
        let mut brain = Brain { config: brain_config, out: brain_out, jobs, agent: None, last_reply_at: None, last_reply_user: None, next_consider_at: now, started: now };
        while let Ok(first) = receiver.recv() {
            let mut events = vec![first];
            events.extend(receiver.try_iter());
            for event in coalesce(events) { brain.handle(event); }
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
                std::fs::write(dir.join("settings.json"), serde_json::json!({ "shellPath": config.shell }).to_string()).map_err(|error| error.to_string())?;
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
    Ok(Bot { sender, stop })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompts_tell_the_agent_where_the_message_came_from() {
        assert_eq!(turn_prompt(Mode::Voice, 1, "Jake", "hi"), "[voice call] Jake: hi");
        assert_eq!(turn_prompt(Mode::Text, 42, "Jake", "hi"), "[text channel 42] Jake: hi");
    }

    #[test]
    fn merges_consecutive_utterances_from_the_same_speaker_only() {
        let voice = |user: u64, pcm: Vec<i16>| Incoming::Voice { guild: 1, channel: 2, user, speaker: "x".into(), pcm };
        let out = coalesce(vec![voice(7, vec![1, 2]), voice(7, vec![3]), voice(8, vec![4]), voice(7, vec![5])]);
        let pcms: Vec<_> = out.iter().map(|event| match event { Incoming::Voice { pcm, .. } => pcm.clone(), _ => vec![] }).collect();
        assert_eq!(pcms, vec![vec![1, 2, 3], vec![4], vec![5]]);
    }
}
