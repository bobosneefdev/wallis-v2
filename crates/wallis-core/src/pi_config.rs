//! Files Pi reads from a role's config dir: installed packages, the OpenAI-compatible provider and its key.
use serde_json::{json, Value};
use std::path::Path;
use std::process::{Command, Stdio};

/// Name Pi knows the OpenAI-compatible provider by (`RoleConfig.provider`).
pub const CUSTOM: &str = "custom";

/// Pi package sources Wallis accepts: registry, git and https only. The text reaches a command line, so no flags or whitespace.
pub fn valid_package(source: &str) -> Result<(), String> {
    let ok = source.len() <= 200 && ["npm:", "git:", "https://"].iter().any(|prefix| source.starts_with(prefix)) && !source.chars().any(|c| c.is_whitespace() || c.is_control());
    if ok { Ok(()) } else { Err(format!("Pi package '{source}' must start with npm:, git: or https:// and contain no spaces.")) }
}

fn read_object(path: &Path) -> Result<Value, String> {
    match std::fs::read_to_string(path) {
        Ok(text) if !text.trim().is_empty() => serde_json::from_str(&text).map_err(|error| format!("{} is invalid: {error}", path.display())),
        _ => Ok(json!({})),
    }
}

fn write(path: &Path, value: &Value) -> Result<(), String> {
    std::fs::write(path, value.to_string()).map_err(|error| format!("Unable to write {}: {error}", path.display()))
}

/// Merges `settings` (e.g. the worker's `shellPath`) and the package list into `<dir>/settings.json`.
pub fn write_settings(dir: &Path, packages: &[String], settings: Value) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
    let path = dir.join("settings.json");
    let mut root = read_object(&path)?;
    let map = root.as_object_mut().ok_or("Pi settings.json is not an object.")?;
    map.insert("packages".into(), json!(packages));
    if let Some(extra) = settings.as_object() { map.extend(extra.clone()); }
    write(&path, &root)
}

/// Registers the OpenAI-compatible endpoint (`models.json`) with exactly these models, and stores its key in `auth.json`.
pub fn write_provider(dir: &Path, base_url: &str, api_key: &str, models: &[String]) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
    let list: Vec<Value> = models.iter().map(|id| json!({ "id": id, "reasoning": true })).collect();
    write(&dir.join("models.json"), &json!({ "providers": { CUSTOM: { "baseUrl": base_url, "api": "openai-completions", "models": list } } }))?;
    let path = dir.join("auth.json");
    let mut root = read_object(&path)?;
    let map = root.as_object_mut().ok_or("Pi auth.json is not an object.")?;
    if api_key.is_empty() { map.remove(CUSTOM); } else { map.insert(CUSTOM.into(), json!({ "type": "api_key", "key": api_key })); }
    write(&path, &root)
}

/// Installs each package into the role's own Pi dir (Pi runs `--offline`, so this is the only time it touches the network).
pub fn install_packages(pi_program: &str, dir: &Path, packages: &[String]) -> Result<(), String> {
    packages.iter().try_for_each(|source| {
        valid_package(source)?;
        let output = Command::new(pi_program).args(["install", source]).env("PI_CODING_AGENT_DIR", dir).stdin(Stdio::null()).output().map_err(|error| format!("Unable to run Pi: {error}"))?;
        if output.status.success() { Ok(()) } else { Err(format!("Installing {source} failed: {}", String::from_utf8_lossy(&output.stderr).trim())) }
    })
}

/// `GET {base}/models` as OpenAI-compatible servers answer it: model ids plus, per model and only when the server exposes them, reasoning levels.
#[derive(Debug, Default, PartialEq, serde::Serialize)]
pub struct ProviderModels { pub models: Vec<String>, pub efforts: std::collections::BTreeMap<String, Vec<String>> }

pub fn parse_models(body: &str) -> Result<ProviderModels, String> {
    let root: Value = serde_json::from_str(body).map_err(|_| "The provider did not return JSON from /models.".to_string())?;
    let items = root.get("data").and_then(Value::as_array).ok_or("The provider's /models answer has no model list.")?;
    let strings = |value: Option<&Value>| value.and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).map(String::from).collect::<Vec<_>>();
    let mut out = ProviderModels::default();
    for item in items {
        let Some(id) = item.get("id").and_then(Value::as_str) else { continue };
        out.models.push(id.into());
        let levels = strings(item.get("reasoning_efforts").or_else(|| item.get("supported_reasoning_efforts")));
        if !levels.is_empty() { out.efforts.insert(id.into(), levels); }
    }
    out.models.sort();
    Ok(out)
}

pub fn fetch_models(base_url: &str, api_key: &str) -> Result<ProviderModels, String> {
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let mut request = ureq::get(&url).config().timeout_global(Some(std::time::Duration::from_secs(15))).build();
    if !api_key.is_empty() { request = request.header("Authorization", &format!("Bearer {api_key}")); }
    let body = request.call().map_err(|error| format!("Unable to list models from {url}: {error}"))?.body_mut().read_to_string().map_err(|error| error.to_string())?;
    parse_models(&body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_registry_git_and_https_package_sources_pass() {
        for ok in ["npm:@scope/pkg@1.2.3", "git:github.com/user/repo@v1", "https://github.com/user/repo"] { assert!(valid_package(ok).is_ok(), "{ok}"); }
        for bad in ["--flag", "/etc/passwd", "npm:pkg; rm -rf /", "npm: pkg", ""] { assert!(valid_package(bad).is_err(), "{bad}"); }
    }

    #[test]
    fn reads_models_and_reasoning_levels_only_when_the_provider_exposes_them() {
        let plain = parse_models(r#"{"data":[{"id":"b"},{"id":"a"}]}"#).unwrap();
        assert_eq!((plain.models, plain.efforts.is_empty()), (vec!["a".to_string(), "b".into()], true));
        let rich = parse_models(r#"{"data":[{"id":"m","reasoning_efforts":["low","high"]},{"id":"n","reasoning_efforts":["high"]}]}"#).unwrap();
        assert_eq!((rich.efforts["m"].clone(), rich.efforts["n"].clone()), (vec!["low".to_string(), "high".into()], vec!["high".to_string()]));
        assert!(parse_models("<html>").is_err() && parse_models("{}").is_err());
    }

    #[test]
    fn provider_files_merge_with_existing_credentials_and_settings() {
        let dir = std::env::temp_dir().join(format!("wallis-piconfig-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("auth.json"), r#"{"openai-codex":{"type":"oauth"}}"#).unwrap();
        write_provider(&dir, "http://localhost:1/v1", "sk-1", &["m".into()]).unwrap();
        let auth: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("auth.json")).unwrap()).unwrap();
        assert_eq!((auth["custom"]["key"].as_str(), auth["openai-codex"]["type"].as_str()), (Some("sk-1"), Some("oauth")));
        write_provider(&dir, "http://localhost:1/v1", "", &["m".into()]).unwrap();
        assert!(std::fs::read_to_string(dir.join("auth.json")).unwrap().contains("openai-codex") && !std::fs::read_to_string(dir.join("auth.json")).unwrap().contains("sk-1"));
        write_settings(&dir, &["npm:x".into()], json!({ "shellPath": "sh" })).unwrap();
        write_settings(&dir, &[], json!({})).unwrap();
        let settings: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("settings.json")).unwrap()).unwrap();
        assert_eq!((settings["shellPath"].as_str(), settings["packages"].as_array().map(Vec::len)), (Some("sh"), Some(0)));
        std::fs::remove_dir_all(&dir).ok();
    }
}
