//! System prompts and per-role Pi config files. Memory exists only in the realtime role's MCP config.
use serde_json::{json, Value};

/// `persona` is the user's Wallis soul document (name, bio, voice).
pub fn realtime_prompt(persona: &str) -> String {
    format!("{persona}\n\n\
You are talking live in a Discord voice call and text channels. Speak only through tools: `say` for one short spoken sentence at a time, \
`message` or `file` for text and code (never speak code aloud), `skip` to stay silent. Keep replies brief; this is a conversation.\n\
You have long-term memory tools: recall before answering anything that may depend on the past, and store durable facts about people and plans yourself.\n\
For coding, research or other heavy work, call `delegate`. The worker is a separate agent that CANNOT see your memory or this conversation, \
so put everything it needs in `context`: recalled facts, names, paths, preferences, and the exact outcome wanted. Say aloud that you handed it off, then keep talking.")
}

pub fn worker_prompt() -> String {
    "You are a background worker for Wallis. You receive one task with its context and work in a sandboxed scratch folder using your file and shell tools. \
You have no memory and no access to the conversation beyond what the task says. Finish the task, then reply with a concise result: what you did, where the files are, and anything the requester must decide."
        .into()
}

/// Pi `mcp.json` for the realtime role: the Mnemosyne stdio server. `python` is the embedded interpreter.
pub fn realtime_mcp(python: &str, data_dir: &str) -> Value {
    json!({ "mcpServers": { "mnemosyne": { "command": python, "args": ["-m", "mnemosyne", "mcp"], "env": { "MNEMOSYNE_DATA_DIR": data_dir } } } })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn realtime_knows_the_worker_is_memory_blind() {
        let prompt = realtime_prompt("You are Wallis.");
        assert!(prompt.starts_with("You are Wallis.") && prompt.contains("CANNOT see your memory") && prompt.contains("`context`"));
    }

    #[test]
    fn worker_prompt_never_mentions_tools_it_lacks() {
        let prompt = worker_prompt();
        assert!(!prompt.contains("`say`") && !prompt.contains("`delegate`") && prompt.contains("no memory"));
    }

    #[test]
    fn memory_server_is_configured_for_the_realtime_role_only() {
        let config = realtime_mcp("py.exe", "C:/data");
        assert_eq!(config["mcpServers"]["mnemosyne"]["args"], json!(["-m", "mnemosyne", "mcp"]));
        assert_eq!(config["mcpServers"]["mnemosyne"]["env"]["MNEMOSYNE_DATA_DIR"], "C:/data");
    }
}
