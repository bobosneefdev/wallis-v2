//! Platform-independent core for Wallis: provider catalogs, per-role agent settings, Pi RPC and jobs.
pub mod agent;
pub mod catalog;
pub mod codex_auth;
pub mod host;
pub mod jobs;
pub mod participation;
pub mod pi;
pub mod pi_config;
pub mod prompts;
pub mod provision;
pub mod role;
#[cfg(windows)]
pub mod sandbox;
pub mod speech;
pub mod stats;
pub mod update;
pub mod utterance;
pub mod worker;
