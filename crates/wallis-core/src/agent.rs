//! One running Pi process in RPC mode: send a prompt, stream text, answer the extension's host calls.
use crate::pi::{self, Event, HostCall, LineSplitter};
use crate::role::{Role, RoleConfig};
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

pub struct Agent { handle: Handle, stdin: Box<dyn Write + Send>, lines: Receiver<String>, next_id: u64 }

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
        Ok(Self { handle, stdin, lines, next_id: 0 })
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
        self.next_id += 1;
        let id = format!("wallis-{}", self.next_id);
        self.send(&json!({ "id": id, "type": "prompt", "message": message }))?;
        let (mut reply, mut last_event) = (String::new(), Instant::now());
        let started = Instant::now();
        loop {
            if self.handle.confined {
                crate::mods::check_budget(Path::new("/var/lib/wallis"))?;
                if started.elapsed() > Duration::from_secs(300) { return Err("Worker timed out.".into()); }
            }
            for text in steer() { self.send(&json!({ "type": "steer", "message": text }))?; }
            let line = match self.lines.recv_timeout(POLL.min(timeout)) {
                Ok(line) => { last_event = Instant::now(); line }
                Err(RecvTimeoutError::Timeout) if last_event.elapsed() < timeout => continue,
                Err(RecvTimeoutError::Timeout) => { let _ = self.send(&json!({ "type": "abort" })); return Err("Pi stopped responding.".into()); }
                Err(RecvTimeoutError::Disconnected) => return Err("Pi exited unexpectedly.".into()),
            };
            if let Some(call) = pi::parse_host_call(&line) {
                let request_id = call.request_id.clone();
                let result = on_host(call);
                self.send(&pi::host_reply(&request_id, &result))?;
                continue;
            }
            match pi::parse_event(&line)? {
                Event::TextDelta(delta) => { on_delta(&delta); reply.push_str(&delta); }
                Event::Response { id: Some(rid), success: false, error } if rid == id => return Err(error.unwrap_or_else(|| "Pi rejected the prompt.".into())),
                Event::Settled { aborted } => return if aborted { Err("The turn was aborted.".into()) } else { Ok(reply) },
                _ => {}
            }
        }
    }

    pub fn abort(&mut self) { let _ = self.send(&json!({ "type": "abort" })); }
}

impl Drop for Agent {
    fn drop(&mut self) { crate::sandbox::kill(&mut self.handle.child, self.handle.confined); }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// A stand-in for Pi: streams text, asks the host for `say`, waits for the answer, then settles.
    const FAKE_PI: &str = r#"#!/bin/sh
read prompt
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
        let dir = std::env::temp_dir().join(format!("wallis-agent-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("fake-pi.sh");
        std::fs::write(&script, FAKE_PI).unwrap();
        { use std::os::unix::fs::PermissionsExt; std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap(); }
        let config = RoleConfig { provider: "openai-codex".into(), model: "gpt-6-sol".into(), effort: "low".into(), ..RoleConfig::default() };
        let mut agent = Agent::spawn(script.to_str().unwrap(), Role::Realtime, &config, &dir.join("pi"), "system", &dir, None).unwrap();
        let (mut calls, mut streamed) = (vec![], String::new());
        let reply = agent.turn("hello", Duration::from_secs(5), &mut |call| { calls.push((call.tool, call.args)); "spoken".into() }, &mut |delta| streamed.push_str(delta)).unwrap();
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
        let dir = std::env::temp_dir().join(format!("wallis-agent-steer-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("steerable.sh");
        std::fs::write(&script, STEERABLE_PI).unwrap();
        { use std::os::unix::fs::PermissionsExt; std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap(); }
        let config = RoleConfig { provider: "p".into(), model: "m".into(), ..RoleConfig::default() };
        let mut agent = Agent::spawn(script.to_str().unwrap(), Role::Realtime, &config, &dir.join("pi"), "s", &dir, None).unwrap();
        let mut sent = false;
        let reply = agent.turn_steered("go", Duration::from_secs(5), &mut || if std::mem::replace(&mut sent, true) { vec![] } else { vec!["look left".into()] }, &mut |_| String::new(), &mut |_| {}).unwrap();
        assert_eq!(reply, "AB");
        drop(agent);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn times_out_when_pi_goes_silent() {
        let dir = std::env::temp_dir().join(format!("wallis-agent-silent-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("silent.sh");
        std::fs::write(&script, "#!/bin/sh\nsleep 5\n").unwrap();
        { use std::os::unix::fs::PermissionsExt; std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap(); }
        let config = RoleConfig { provider: "p".into(), model: "m".into(), ..RoleConfig::default() };
        let mut agent = Agent::spawn(script.to_str().unwrap(), Role::Worker, &config, &dir.join("pi"), "s", &dir, None).unwrap();
        assert!(agent.turn("x", Duration::from_millis(200), &mut |_| String::new(), &mut |_| {}).unwrap_err().contains("stopped responding"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
