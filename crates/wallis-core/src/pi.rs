//! Pi RPC: launch arguments and JSONL framing/events. Pure functions so they test without a Pi install.
use crate::role::{Role, RoleConfig};
use serde_json::Value;

/// Realtime Pi: no shell or file tools, only Wallis' own tools plus MCP (Mnemosyne).
/// Worker Pi: coding tools, and `--no-mcp` so it can never reach memory.
pub fn launch_args(role: Role, config: &RoleConfig, extension: &str, system_prompt: &str) -> Vec<String> {
    let mut args: Vec<String> = ["--mode", "rpc", "--no-session", "--no-approve", "--no-skills", "--no-prompt-templates", "--no-context-files", "--offline"].map(String::from).to_vec();
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

/// The extension embedded in the app; written next to Pi's per-role config and loaded with `-e`.
pub const EXTENSION: &str = include_str!("../pi/wallis.ts");

/// Environment that tells the extension which role it serves and how to shape Codex requests.
pub fn role_env(role: Role, config: &RoleConfig) -> Vec<(&'static str, String)> {
    vec![
        ("WALLIS_ROLE", if role == Role::Realtime { "realtime" } else { "worker" }.into()),
        ("WALLIS_EFFORT", config.effort.clone()),
        ("WALLIS_FAST", if config.fast { "1" } else { "0" }.into()),
    ]
}

/// A tool call the extension forwards to the host through an `input` dialog.
#[derive(Debug, PartialEq)]
pub struct HostCall { pub request_id: String, pub tool: String, pub args: Value }

const HOST_PREFIX: &str = "wallis-host:";

pub fn parse_host_call(line: &str) -> Option<HostCall> {
    let value: Value = serde_json::from_str(line).ok()?;
    if value.get("type")?.as_str()? != "extension_ui_request" || value.get("method")?.as_str()? != "input" { return None; }
    let payload: Value = serde_json::from_str(value.get("title")?.as_str()?.strip_prefix(HOST_PREFIX)?).ok()?;
    Some(HostCall { request_id: value.get("id")?.as_str()?.into(), tool: payload.get("tool")?.as_str()?.into(), args: payload.get("args").cloned().unwrap_or(Value::Null) })
}

/// Answers a host call; the string becomes the tool result the model sees.
pub fn host_reply(request_id: &str, result: &str) -> Value {
    serde_json::json!({ "type": "extension_ui_response", "id": request_id, "value": result })
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

    fn config() -> RoleConfig { RoleConfig { provider: "openai-codex".into(), model: "gpt-6-sol".into(), effort: "low".into(), fast: true, packages: vec![] } }

    #[test]
    fn worker_cannot_reach_memory_and_realtime_cannot_run_shell() {
        let worker = launch_args(Role::Worker, &config(), "ext.ts", "p");
        assert!(worker.contains(&"--no-mcp".to_string()), "worker must not load the Mnemosyne MCP server");
        assert!(worker.iter().any(|arg| arg.contains("bash")));
        let realtime = launch_args(Role::Realtime, &config(), "ext.ts", "p");
        assert!(!realtime.contains(&"--no-mcp".to_string()), "realtime needs MCP for memory");
        assert!(realtime.contains(&"--no-builtin-tools".to_string()) && !realtime.iter().any(|arg| arg.contains("bash")));
        // `--no-extensions` would also switch off Pi's built-in MCP (memory) and installed packages.
        assert!(!realtime.contains(&"--no-extensions".to_string()) && realtime.contains(&"--no-approve".to_string()));
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
    fn routes_extension_tool_calls_to_the_host_and_ignores_other_dialogs() {
        let line = json!({"type":"extension_ui_request","id":"u1","method":"input","title":"wallis-host:{\"tool\":\"say\",\"args\":{\"text\":\"hi\"}}"}).to_string();
        let call = parse_host_call(&line).unwrap();
        assert_eq!((call.request_id.as_str(), call.tool.as_str(), &call.args), ("u1", "say", &json!({"text":"hi"})));
        assert_eq!(host_reply("u1", "ok")["value"], "ok");
        assert!(parse_host_call(r#"{"type":"extension_ui_request","id":"u2","method":"input","title":"Enter a value"}"#).is_none());
        assert!(parse_host_call(r#"{"type":"extension_ui_request","id":"u3","method":"notify","title":"wallis-host:{}"}"#).is_none());
    }

    #[test]
    fn extension_ships_with_the_tools_and_the_role_gate() {
        for tool in ["\"say\"", "\"stop_speaking\"", "\"message\"", "\"file\"", "\"skip\"", "\"delegate\"", "\"update_profile\""] { assert!(EXTENSION.contains(tool), "{tool}"); }
        assert!(EXTENSION.contains("WALLIS_ROLE") && EXTENSION.contains("service_tier"));
        let env = role_env(Role::Worker, &RoleConfig { fast: true, ..RoleConfig::default() });
        assert!(env.contains(&("WALLIS_ROLE", "worker".to_string())) && env.contains(&("WALLIS_FAST", "1".to_string())));
    }

    #[test]
    fn validates_role_values_that_reach_the_command_line() {
        assert!(config().validate().is_ok());
        assert!(RoleConfig { model: "x; rm -rf /".into(), ..config() }.validate().is_err());
        assert!(RoleConfig::default().validate().is_err());
    }
}
