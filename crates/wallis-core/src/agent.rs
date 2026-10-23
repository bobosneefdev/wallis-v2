//! One running Pi process in RPC mode: send a prompt, stream text, answer the extension's host calls.
use crate::pi::{self, Event, HostCall, LineSplitter};
use crate::role::{Role, RoleConfig};
use crate::stats::{log, secs};
use serde_json::json;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

/// What a confined worker may touch: folders it can read (runtimes), folders it can write, and its `PATH`.
pub struct Access { pub read: Vec<PathBuf>, pub write: Vec<PathBuf>, pub path: Vec<PathBuf> }

/// Keeps the process alive and kills it on drop: a plain child, or a confined Linux process group.
struct Handle { child: Child, confined: bool }

/// How often a running turn looks for steering messages.
const POLL: Duration = Duration::from_millis(100);

pub struct Agent { handle: Handle, stdin: Box<dyn Write + Send>, lines: Receiver<String>, next_id: u64,
    /// `realtime` or `worker`: which agent a live-log line is about.
    role: &'static str,
    /// The longest one turn may spend waiting on the model before it is aborted (the worker's is five minutes). Time spent
    /// running Wallis' own tools (a song download, joining a call) does not count.
    pub limit: Duration }

impl Agent {
    /// Starts Pi for `role`. `dir` is the role's own Pi config dir (auth, settings, MCP config) and also holds the extension file.
    /// With `confine`, Pi runs as the worker uid inside a private Linux mount/process namespace.
    pub fn spawn(program: &str, role: Role, config: &RoleConfig, dir: &Path, system_prompt: &str, cwd: &Path, confine: Option<&Access>) -> Result<Self, String> {
        config.validate()?;
        if confine.is_none() { std::fs::create_dir_all(dir).map_err(|error| format!("Unable to create {}: {error}", dir.display()))?;
        let extension = dir.join("wallis.ts");
        std::fs::write(&extension, pi::EXTENSION).map_err(|error| format!("Unable to write the Pi extension: {error}"))?;
        }
        let extension = dir.join("wallis.ts");
        let mut args = pi::launch_args(role, config, &extension.to_string_lossy(), system_prompt);
        if role == Role::Worker {
            for name in ["effect", "wallis-mods", "music", "browser"] {
                args.extend(["--skill".into(), format!("/opt/wallis/tools/skills/{name}/SKILL.md")]);
            }
        }
        let mut env: Vec<(String, String)> = pi::role_env(role, config).into_iter().map(|(k,v)| (k.into(),v)).collect();
        env.push(("PI_CODING_AGENT_DIR".into(), dir.to_string_lossy().into()));
        let mut command = match confine {
            Some(access) => crate::sandbox::command(Path::new(program), &args, cwd, &env, access, false)?,
            None => { let mut c = Command::new(program); c.args(&args).envs(env).current_dir(cwd); c },
        };
        #[cfg(target_os = "linux")]
        { use std::os::unix::process::CommandExt; command.process_group(0); }
        command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit());
        let mut child = command.spawn().map_err(|error| format!("Unable to start Pi ({program}): {error}"))?;
        let stdin: Box<dyn Write + Send> = Box::new(child.stdin.take().ok_or("Pi has no stdin")?);
        let stdout: Box<dyn Read + Send> = Box::new(child.stdout.take().ok_or("Pi has no stdout")?);
        let handle = Handle { child, confined: confine.is_some() };
        let mut stdout = stdout;
        let (sender, lines) = channel();
        std::thread::spawn(move || {
            let (mut splitter, mut buffer) = (LineSplitter::default(), [0u8; 8192]);
            while let Ok(read) = stdout.read(&mut buffer) {
                if read == 0 { break; }
                for line in splitter.push(&buffer[..read]) { if sender.send(line).is_err() { return; } }
            }
        });
        Ok(Self { handle, stdin, lines, next_id: 0, limit: Duration::from_secs(300), role: if role == Role::Realtime { "realtime" } else { "worker" } })
    }

    fn send(&mut self, value: &serde_json::Value) -> Result<(), String> {
        self.stdin.write_all(pi::command_line(value).as_bytes()).and_then(|_| self.stdin.flush()).map_err(|error| format!("Pi is not accepting input: {error}"))
    }

    /// Runs one prompt to completion. `on_host` answers tool calls (its string is the tool result); `on_delta` sees streamed text.
    /// Returns the full reply text. `timeout` bounds the silence between Pi events, not the whole turn.
    pub fn turn(&mut self, message: &str, timeout: Duration, on_host: &mut dyn FnMut(HostCall) -> String, on_delta: &mut dyn FnMut(&str)) -> Result<String, String> {
        self.turn_steered(message, timeout, &mut Vec::new, on_host, on_delta)
    }

    /// Like `turn`, but `steer` is polled while Pi works and each message it returns is queued into the running turn:
    /// Pi delivers it after the current tool calls, before its next model call, without aborting anything.
    /// ponytail: a steer sent in the instant Pi is already settling waits in Pi's queue for the next prompt instead of being lost.
    pub fn turn_steered(&mut self, message: &str, timeout: Duration, steer: &mut dyn FnMut() -> Vec<String>, on_host: &mut dyn FnMut(HostCall) -> String, on_delta: &mut dyn FnMut(&str)) -> Result<String, String> {
        self.turn_with_images(message, &[], timeout, steer, on_host, on_delta)
    }

    /// Images use Pi's native ImageContent format; URLs in prompt text are not visual inputs.
    pub fn turn_with_images(&mut self, message: &str, images: &[crate::media::Image], timeout: Duration, steer: &mut dyn FnMut() -> Vec<String>, on_host: &mut dyn FnMut(HostCall) -> String, on_delta: &mut dyn FnMut(&str)) -> Result<String, String> {
        self.next_id += 1;
        let id = format!("wallis-{}", self.next_id);
        self.send(&json!({ "id": id, "type": "prompt", "message": message, "images": images }))?;
        let (mut reply, mut last_event) = (String::new(), Instant::now());
        let (started, mut overdue, mut tools) = (Instant::now(), false, Duration::ZERO);
        // When the current model request started (for the live log), and when its first token arrived.
        let (mut asked, mut first) = (started, None::<Duration>);
        loop {
            if self.handle.confined {
                crate::mods::check_budget(Path::new("/var/lib/wallis"))?;
                if started.elapsed() > self.limit { return Err("Worker timed out.".into()); }
            }
            // Past the limit, a live agent aborts and lets Pi settle, so it keeps its context for the next turn.
            // Queued steers survive an abort in Pi, so drop them first: what was said during this turn is stale by now.
            if !overdue && started.elapsed().saturating_sub(tools) > self.limit {
                overdue = true;
                log("limit", format!("{} turn passed {} of model time; stopping it", self.role, secs(self.limit)));
                self.send(&json!({ "type": "clear_queue" }))?; self.abort();
            }
            for text in steer() { self.send(&json!({ "type": "steer", "message": text }))?; }
            let line = match self.lines.recv_timeout(POLL.min(timeout)) {
                Ok(line) => { last_event = Instant::now(); line }
                Err(RecvTimeoutError::Timeout) if last_event.elapsed() < timeout => continue,
                Err(RecvTimeoutError::Timeout) => { let _ = self.send(&json!({ "type": "abort" })); log("error", format!("{} Pi silent for {}; aborted", self.role, secs(timeout))); return Err("Pi stopped responding.".into()); }
                Err(RecvTimeoutError::Disconnected) => return Err("Pi exited unexpectedly.".into()),
            };
            if let Some(call) = pi::parse_host_call(&line) {
                let request_id = call.request_id.clone();
                let (running, tool) = (Instant::now(), call.tool.clone());
                let result = on_host(call);
                (tools, last_event) = (tools + running.elapsed(), Instant::now());
                let failed = if result.starts_with("Failed:") { format!(" — {result}") } else { String::new() };
                log("tool", format!("{} {tool} {}{failed}", self.role, secs(running.elapsed())));
                self.send(&pi::host_reply(&request_id, &result))?;
                continue;
            }
            pi::progress(&line, &mut |note| match note {
                pi::Progress::Asked => (asked, first) = (Instant::now(), None),
                pi::Progress::Token => { first.get_or_insert(asked.elapsed()); }
                pi::Progress::Answered(summary) => log("model", format!("{} {} (first token {}) {summary}", self.role, secs(asked.elapsed()), first.map(secs).unwrap_or_else(|| "—".into()))),
                pi::Progress::Note(tag, text) => log(tag, format!("{} {text}", self.role)),
            });
            match pi::parse_event(&line)? {
                Event::TextDelta(delta) => { on_delta(&delta); reply.push_str(&delta); }
                Event::Response { id: Some(rid), success: false, error } if rid == id => return Err(error.unwrap_or_else(|| "Pi rejected the prompt.".into())),
                Event::Settled { aborted } => return if overdue { Err("The turn ran too long and was stopped.".into()) } else if aborted { Err("The turn was aborted.".into()) } else { Ok(reply) },
                _ => {}
            }
        }
    }

    pub fn abort(&mut self) { let _ = self.send(&json!({ "type": "abort" })); }

    /// Sends one RPC command between turns and returns its response's `data`.
    pub fn command(&mut self, kind: &str, timeout: Duration) -> Result<serde_json::Value, String> {
        self.next_id += 1;
        let id = format!("wallis-{}", self.next_id);
        self.send(&json!({ "id": id, "type": kind }))?;
        let deadline = Instant::now() + timeout;
        loop {
            let line = self.lines.recv_timeout(deadline.saturating_duration_since(Instant::now())).map_err(|_| format!("Pi did not answer {kind}."))?;
            let value: serde_json::Value = serde_json::from_str(&line).unwrap_or_default();
            if value["type"] == "response" && value["id"] == id.as_str() {
                return if value["success"] == true { Ok(value["data"].clone()) } else { Err(value["error"].as_str().unwrap_or("Pi refused the command.").into()) };
            }
        }
    }
}

impl Drop for Agent {
    fn drop(&mut self) { crate::sandbox::kill(&mut self.handle.child, self.handle.confined); }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// Writes `body` as an executable stand-in for Pi in a fresh temp dir and starts it. Tests run in parallel, and a fork in
    /// another test can briefly inherit this file's write handle, so exec may fail with ETXTBSY: retry until it is released.
    fn fake(name: &str, body: &str, role: Role) -> (std::path::PathBuf, Agent) {
        let dir = std::env::temp_dir().join(format!("wallis-agent-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join(format!("{name}.sh"));
        std::fs::write(&script, body).unwrap();
        { use std::os::unix::fs::PermissionsExt; std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap(); }
        let config = RoleConfig { provider: "openai-codex".into(), model: "gpt-6-sol".into(), effort: "low".into(), ..RoleConfig::default() };
        for _ in 0..100 {
            match Agent::spawn(script.to_str().unwrap(), role, &config, &dir.join("pi"), "system", &dir, None) {
                Err(error) if error.contains("Text file busy") => std::thread::sleep(Duration::from_millis(10)),
                result => return (dir, result.unwrap()),
            }
        }
        panic!("{} stayed busy", script.display());
    }

    /// A stand-in for Pi: streams text, asks the host for `say`, waits for the answer, then settles.
    const FAKE_PI: &str = r#"#!/bin/sh
read prompt
printf '%s' "$prompt" > prompt.json
echo '{"type":"response","id":"wallis-1","success":true}'
echo '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":"Hi "}}'
echo '{"type":"extension_ui_request","id":"u1","method":"input","title":"wallis-host:{\"tool\":\"say\",\"args\":{\"text\":\"hello\"}}"}'
read answer
case "$answer" in *spoken*) echo '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":"there"}}';; esac
echo '{"type":"agent_settled"}'
sleep 5
"#;

    #[test]
    fn runs_a_turn_streams_text_and_answers_host_calls() {
        let (dir, mut agent) = fake("turn", FAKE_PI, Role::Realtime);
        let (mut calls, mut streamed) = (vec![], String::new());
        let images = vec![crate::media::Image { r#type: "image".into(), data: "aGVsbG8=".into(), mime_type: "image/png".into() }];
        let reply = agent.turn_with_images("hello", &images, Duration::from_secs(5), &mut Vec::new, &mut |call| { calls.push((call.tool, call.args)); "spoken".into() }, &mut |delta| streamed.push_str(delta)).unwrap();
        let prompt: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dir.join("prompt.json")).unwrap()).unwrap();
        assert_eq!(prompt["images"], json!([{"type":"image","data":"aGVsbG8=","mimeType":"image/png"}]));
        assert_eq!(prompt["message"], "hello");
        assert_eq!(reply, "Hi there");
        assert_eq!(streamed, reply);
        assert_eq!(calls, vec![("say".to_string(), json!({ "text": "hello" }))]);
        drop(agent);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Waits for a `steer` command mid-turn, like Pi does, and only then finishes.
    const STEERABLE_PI: &str = r#"#!/bin/sh
read prompt
echo '{"type":"response","id":"wallis-1","success":true}'
echo '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":"A"}}'
read steer
case "$steer" in *'look left'*) echo '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":"B"}}';; esac
echo '{"type":"agent_settled"}'
sleep 5
"#;

    #[test]
    fn steering_reaches_a_running_turn_without_aborting_it() {
        let (dir, mut agent) = fake("steer", STEERABLE_PI, Role::Realtime);
        let mut sent = false;
        let reply = agent.turn_steered("go", Duration::from_secs(5), &mut || if std::mem::replace(&mut sent, true) { vec![] } else { vec!["look left".into()] }, &mut |_| String::new(), &mut |_| {}).unwrap();
        assert_eq!(reply, "AB");
        drop(agent);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Keeps "thinking" until it is told to abort, then settles like Pi does.
    const SLOW_PI: &str = r#"#!/bin/sh
read prompt
echo '{"type":"response","id":"wallis-1","success":true}'
while read line; do case "$line" in *abort*) echo '{"type":"agent_settled","aborted":true}';; esac; done
"#;

    #[test]
    fn a_turn_past_its_limit_is_aborted_but_the_agent_survives() {
        let (dir, mut agent) = fake("slow", SLOW_PI, Role::Realtime);
        agent.limit = Duration::from_millis(300);
        let started = Instant::now();
        assert!(agent.turn("x", Duration::from_secs(5), &mut |_| String::new(), &mut |_| {}).unwrap_err().contains("too long"));
        assert!(started.elapsed() < Duration::from_secs(2));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn times_out_when_pi_goes_silent() {
        let (dir, mut agent) = fake("silent", "#!/bin/sh\nsleep 5\n", Role::Worker);
        assert!(agent.turn("x", Duration::from_millis(200), &mut |_| String::new(), &mut |_| {}).unwrap_err().contains("stopped responding"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
