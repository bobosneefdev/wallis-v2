//! Pi RPC: launch arguments and JSONL framing/events. Pure functions so they test without a Pi install.
use crate::role::{Role, RoleConfig};
use serde_json::Value;

/// Realtime Pi: no shell or file tools, only Wallis' own tools plus MCP (Mnemosyne).
/// Worker Pi: coding tools, and `--no-mcp` so it can never reach memory.
pub fn launch_args(role: Role, config: &RoleConfig, extension: &str, system_prompt: &str) -> Vec<String> {
    let mut args: Vec<String> = ["--mode", "rpc", "--no-session", "--no-extensions", "--no-skills", "--no-prompt-templates", "--no-context-files", "--offline"].map(String::from).to_vec();
    args.extend(["-e", extension, "--provider", &config.provider, "--model", &config.model, "--thinking", &config.effort, "--system-prompt", system_prompt].map(String::from));
    match role {
        Role::Realtime => args.extend(["--no-builtin-tools"].map(String::from)),
        Role::Worker => args.extend(["--no-mcp", "--tools", "read,bash,edit,write,grep,find,ls"].map(String::from)),
    }
    args
}

/// One RPC command as a single LF-terminated line (Pi's strict JSONL framing).
pub fn command_line(value: &Value) -> String { format!("{value}\n") }

/// Splits a byte stream on LF only. `readline`-style splitting on U+2028/U+2029 corrupts JSON strings.
#[derive(Default)]
pub struct LineSplitter { buffer: Vec<u8> }

impl LineSplitter {
    pub fn push(&mut self, bytes: &[u8]) -> Vec<String> {
        self.buffer.extend_from_slice(bytes);
        let mut lines = Vec::new();
        while let Some(end) = self.buffer.iter().position(|&byte| byte == b'\n') {
            let mut line: Vec<u8> = self.buffer.drain(..=end).collect();
            line.pop();
            if line.last() == Some(&b'\r') { line.pop(); }
            if !line.is_empty() { lines.push(String::from_utf8_lossy(&line).into_owned()); }
        }
        lines
    }
}

#[derive(Debug, PartialEq)]
pub enum Event {
    TextDelta(String),
    ToolStart { id: String, name: String, args: Value },
    ToolEnd { id: String, name: String, is_error: bool },
    Settled { aborted: bool },
    Response { id: Option<String>, success: bool, error: Option<String> },
    Other,
}

pub fn parse_event(line: &str) -> Result<Event, String> {
    let value: Value = serde_json::from_str(line).map_err(|error| format!("Pi sent invalid JSON: {error}"))?;
    let text = |key: &str| value.get(key).and_then(Value::as_str).map(String::from);
    Ok(match value.get("type").and_then(Value::as_str) {
        Some("message_update") => match value.pointer("/assistantMessageEvent/type").and_then(Value::as_str) {
            Some("text_delta") => Event::TextDelta(value.pointer("/assistantMessageEvent/delta").and_then(Value::as_str).unwrap_or_default().into()),
            _ => Event::Other,
        },
        Some("tool_execution_start") => Event::ToolStart { id: text("toolCallId").unwrap_or_default(), name: text("toolName").unwrap_or_default(), args: value.get("args").cloned().unwrap_or(Value::Null) },
        Some("tool_execution_end") => Event::ToolEnd { id: text("toolCallId").unwrap_or_default(), name: text("toolName").unwrap_or_default(), is_error: value.get("isError").and_then(Value::as_bool).unwrap_or(false) },
        Some("agent_settled") => Event::Settled { aborted: value.get("aborted").and_then(Value::as_bool).unwrap_or(false) },
        Some("response") => Event::Response { id: text("id"), success: value.get("success").and_then(Value::as_bool).unwrap_or(false), error: text("error") },
        _ => Event::Other,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn config() -> RoleConfig { RoleConfig { provider: "openai-codex".into(), model: "gpt-6-sol".into(), effort: "low".into(), fast: true } }

    #[test]
    fn worker_cannot_reach_memory_and_realtime_cannot_run_shell() {
        let worker = launch_args(Role::Worker, &config(), "ext.ts", "p");
        assert!(worker.contains(&"--no-mcp".to_string()), "worker must not load the Mnemosyne MCP server");
        assert!(worker.iter().any(|arg| arg.contains("bash")));
        let realtime = launch_args(Role::Realtime, &config(), "ext.ts", "p");
        assert!(!realtime.contains(&"--no-mcp".to_string()), "realtime needs MCP for memory");
        assert!(realtime.contains(&"--no-builtin-tools".to_string()) && !realtime.iter().any(|arg| arg.contains("bash")));
    }

    #[test]
    fn splits_on_lf_only_across_chunks() {
        let mut splitter = LineSplitter::default();
        assert!(splitter.push(b"{\"a\":\"x\xe2\x80").is_empty());
        assert_eq!(splitter.push(b"\xa8y\"}\r\n{\"b\":1}\n"), vec!["{\"a\":\"x\u{2028}y\"}", "{\"b\":1}"]);
    }

    #[test]
    fn parses_the_events_the_bot_acts_on() {
        let delta = json!({"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":"Hi"}}).to_string();
        assert_eq!(parse_event(&delta).unwrap(), Event::TextDelta("Hi".into()));
        let start = json!({"type":"tool_execution_start","toolCallId":"c1","toolName":"say","args":{"text":"yo"}}).to_string();
        assert_eq!(parse_event(&start).unwrap(), Event::ToolStart { id: "c1".into(), name: "say".into(), args: json!({"text":"yo"}) });
        assert_eq!(parse_event(r#"{"type":"agent_settled","aborted":false}"#).unwrap(), Event::Settled { aborted: false });
        assert_eq!(parse_event(r#"{"type":"turn_end"}"#).unwrap(), Event::Other);
        assert!(parse_event("not json").is_err());
    }

    #[test]
    fn validates_role_values_that_reach_the_command_line() {
        assert!(config().validate().is_ok());
        assert!(RoleConfig { model: "x; rm -rf /".into(), ..config() }.validate().is_err());
        assert!(RoleConfig::default().validate().is_err());
    }
}
