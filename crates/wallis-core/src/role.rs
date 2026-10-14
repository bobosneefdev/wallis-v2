//! Per-role Pi settings. Realtime and worker are configured independently; only realtime gets memory.
use serde::{Deserialize, Serialize};

pub const THINKING_LEVELS: [&str; 7] = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role { Realtime, Worker }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RoleConfig {
    /// Pi provider id, e.g. `openai-codex` (ChatGPT sign-in) or `openrouter`.
    pub provider: String,
    pub model: String,
    /// Pi thinking level, or a Codex-only level such as `ultra` passed through as-is.
    pub effort: String,
    /// Codex Fast mode (`service_tier: priority`). Ignored by providers without it.
    pub fast: bool,
}

impl Default for RoleConfig {
    fn default() -> Self { Self { provider: String::new(), model: String::new(), effort: "medium".into(), fast: false } }
}

fn safe(value: &str, extra: &[u8], max: usize) -> bool {
    !value.is_empty() && value.len() <= max && value.bytes().all(|byte| byte.is_ascii_alphanumeric() || extra.contains(&byte))
}

impl RoleConfig {
    pub fn is_configured(&self) -> bool { !self.provider.is_empty() && !self.model.is_empty() }

    /// Values land on a command line, so only plain identifier characters are allowed.
    pub fn validate(&self) -> Result<(), String> {
        if !safe(&self.provider, b"._-", 80) { return Err("Choose a provider.".into()); }
        if !safe(&self.model, b"._:-/", 120) { return Err("Pick a model, or type a model id such as gpt-6-sol.".into()); }
        if !safe(&self.effort, b"", 16) { return Err("Pick a reasoning level.".into()); }
        Ok(())
    }
}
