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

/// Keeps the process alive and kills it on drop: a plain child, or a job object around a sandboxed one.
enum Handle { Child(Child), #[cfg(windows)] Sandboxed(#[allow(dead_code)] crate::sandbox::Sandboxed) }

/// How often a running turn looks for steering messages.
const POLL: Duration = Duration::from_millis(100);

pub struct Agent { handle: Handle, stdin: Box<dyn Write + Send>, lines: Receiver<String>, next_id: u64 }

impl Agent {
    /// Starts Pi for `role`. `dir` is the role's own Pi config dir (auth, settings, MCP config) and also holds the extension file.
    /// With `confine`, Pi runs inside the Windows sandbox and can only reach those folders.
    pub fn spawn(program: &str, role: Role, config: &RoleConfig, dir: &Path, system_prompt: &str, cwd: &Path, confine: Option<&Access>) -> Result<Self, String> {
        config.validate()?;
        std::fs::create_dir_all(dir).map_err(|error| format!("Unable to create {}: {error}", dir.display()))?;
        let extension = dir.join("wallis.ts");
        std::fs::write(&extension, pi::EXTENSION).map_err(|error| format!("Unable to write the Pi extension: {error}"))?;
        let args = pi::launch_args(role, config, &extension.to_string_lossy(), system_prompt);
        let (handle, stdin, stdout): (Handle, Box<dyn Write + Send>, Box<dyn Read + Send>) = match confine {
            #[cfg(windows)]
            Some(access) => {
                let mut env: Vec<(String, String)> = pi::role_env(role, config).into_iter().map(|(key, value)| (key.to_string(), value)).collect();
                let system = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
                let path = access.path.iter().map(|p| p.to_string_lossy().into_owned()).chain([format!(r"{system}\System32")]).collect::<Vec<_>>().join(";");
                let home = cwd.to_string_lossy().into_owned();
                env.extend([("PI_CODING_AGENT_DIR", dir.to_string_lossy().into_owned()), ("PATH", path), ("SystemRoot", system), ("TEMP", home.clone()), ("TMP", home.clone()), ("HOME", home.clone()), ("USERPROFILE", home)].map(|(key, value)| (key.to_string(), value)));
                let sandboxed = crate::sandbox::spawn(Path::new(program), &args, cwd, &env, access)?;
                let (stdin, stdout) = (sandboxed.stdin.try_clone().map_err(|error| error.to_string())?, sandboxed.stdout.try_clone().map_err(|error| error.to_string())?);
                (Handle::Sandboxed(sandboxed), Box::new(stdin), Box::new(stdout))
            }
            #[cfg(not(windows))]
            Some(_) => return Err("The worker sandbox is only available on Windows.".into()),
            None => {
                let mut command = Command::new(program);
                command.args(&args).envs(pi::role_env(role, config)).env("PI_CODING_AGENT_DIR", dir).current_dir(cwd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
                #[cfg(windows)]
                { use std::os::windows::process::CommandExt; command.creation_flags(0x0800_0000); }
                let mut child = command.spawn().map_err(|error| format!("Unable to start Pi ({program}): {error}"))?;
                let (stdout, stdin) = (child.stdout.take().ok_or("Pi has no stdout")?, child.stdin.take().ok_or("Pi has no stdin")?);
                (Handle::Child(child), Box::new(stdin), Box::new(stdout))
            }
        };
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
        self.turn_with_images(message, &[], timeout, steer, on_host, on_delta)
    }

    /// Images use Pi's native ImageContent format; URLs in prompt text are not visual inputs.
    pub fn turn_with_images(&mut self, message: &str, images: &[crate::media::Image], timeout: Duration, steer: &mut dyn FnMut() -> Vec<String>, on_host: &mut dyn FnMut(HostCall) -> String, on_delta: &mut dyn FnMut(&str)) -> Result<String, String> {
        self.next_id += 1;
        let id = format!("wallis-{}", self.next_id);
        self.send(&json!({ "id": id, "type": "prompt", "message": message, "images": images }))?;
        let (mut reply, mut last_event) = (String::new(), Instant::now());
        loop {
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
    fn drop(&mut self) { if let Handle::Child(child) = &mut self.handle { let _ = child.kill(); let _ = child.wait(); } }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

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
        let dir = std::env::temp_dir().join(format!("wallis-agent-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("fake-pi.sh");
        std::fs::write(&script, FAKE_PI).unwrap();
        { use std::os::unix::fs::PermissionsExt; std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap(); }
        let config = RoleConfig { provider: "openai-codex".into(), model: "gpt-6-sol".into(), effort: "low".into(), ..RoleConfig::default() };
        let mut agent = Agent::spawn(script.to_str().unwrap(), Role::Realtime, &config, &dir.join("pi"), "system", &dir, None).unwrap();
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
