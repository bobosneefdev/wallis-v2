//! One running Pi process in RPC mode: send a prompt, stream text, answer the extension's host calls.
use crate::pi::{self, Event, HostCall, LineSplitter};
use crate::role::{Role, RoleConfig};
use serde_json::json;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::time::Duration;

pub struct Agent { child: Child, stdin: ChildStdin, lines: Receiver<String>, next_id: u64 }

impl Agent {
    /// Starts Pi for `role`. `dir` is the role's own Pi config dir (auth, settings, MCP config) and also holds the extension file.
    pub fn spawn(program: &str, role: Role, config: &RoleConfig, dir: &Path, system_prompt: &str, cwd: &Path) -> Result<Self, String> {
        config.validate()?;
        std::fs::create_dir_all(dir).map_err(|error| format!("Unable to create {}: {error}", dir.display()))?;
        let extension = dir.join("wallis.ts");
        std::fs::write(&extension, pi::EXTENSION).map_err(|error| format!("Unable to write the Pi extension: {error}"))?;
        let mut command = Command::new(program);
        command.args(pi::launch_args(role, config, &extension.to_string_lossy(), system_prompt)).envs(pi::role_env(role, config))
            .env("PI_CODING_AGENT_DIR", dir).current_dir(cwd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        #[cfg(windows)]
        { use std::os::windows::process::CommandExt; command.creation_flags(0x0800_0000); }
        let mut child = command.spawn().map_err(|error| format!("Unable to start Pi ({program}): {error}"))?;
        let (mut stdout, stdin) = (child.stdout.take().ok_or("Pi has no stdout")?, child.stdin.take().ok_or("Pi has no stdin")?);
        let (sender, lines) = channel();
        std::thread::spawn(move || {
            let (mut splitter, mut buffer) = (LineSplitter::default(), [0u8; 8192]);
            while let Ok(read) = stdout.read(&mut buffer) {
                if read == 0 { break; }
                for line in splitter.push(&buffer[..read]) { if sender.send(line).is_err() { return; } }
            }
        });
        Ok(Self { child, stdin, lines, next_id: 0 })
    }

    fn send(&mut self, value: &serde_json::Value) -> Result<(), String> {
        self.stdin.write_all(pi::command_line(value).as_bytes()).and_then(|_| self.stdin.flush()).map_err(|error| format!("Pi is not accepting input: {error}"))
    }

    /// Runs one prompt to completion. `on_host` answers tool calls (its string is the tool result); `on_delta` sees streamed text.
    /// Returns the full reply text. `timeout` bounds the silence between Pi events, not the whole turn.
    pub fn turn(&mut self, message: &str, timeout: Duration, on_host: &mut dyn FnMut(HostCall) -> String, on_delta: &mut dyn FnMut(&str)) -> Result<String, String> {
        self.next_id += 1;
        let id = format!("wallis-{}", self.next_id);
        self.send(&json!({ "id": id, "type": "prompt", "message": message }))?;
        let mut reply = String::new();
        loop {
            let line = match self.lines.recv_timeout(timeout) {
                Ok(line) => line,
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
    fn drop(&mut self) { let _ = self.child.kill(); let _ = self.child.wait(); }
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
        let config = RoleConfig { provider: "openai-codex".into(), model: "gpt-6-sol".into(), effort: "low".into(), fast: false };
        let mut agent = Agent::spawn(script.to_str().unwrap(), Role::Realtime, &config, &dir.join("pi"), "system", &dir).unwrap();
        let (mut calls, mut streamed) = (vec![], String::new());
        let reply = agent.turn("hello", Duration::from_secs(5), &mut |call| { calls.push((call.tool, call.args)); "spoken".into() }, &mut |delta| streamed.push_str(delta)).unwrap();
        assert_eq!(reply, "Hi there");
        assert_eq!(streamed, reply);
        assert_eq!(calls, vec![("say".to_string(), json!({ "text": "hello" }))]);
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
        let mut agent = Agent::spawn(script.to_str().unwrap(), Role::Worker, &config, &dir.join("pi"), "s", &dir).unwrap();
        assert!(agent.turn("x", Duration::from_millis(200), &mut |_| String::new(), &mut |_| {}).unwrap_err().contains("stopped responding"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
