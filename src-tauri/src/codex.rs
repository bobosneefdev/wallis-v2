//! ChatGPT (Codex) sign-in and the live model catalog, exposed to Settings. Tokens live in each role's Pi
//! config dir (`<app data>/pi/<role>/auth.json`) so Pi uses and refreshes them itself.
use std::path::{Path, PathBuf};
use std::time::Duration;
use tauri::{Manager, State};
use wallis_core::{catalog, codex_auth};

use crate::AppState;

const ROLES: [&str; 2] = ["realtime", "worker"];

pub fn role_dir(app_data: &Path, role: &str) -> PathBuf { app_data.join("pi").join(role) }

fn app_data(state: &AppState) -> Result<PathBuf, String> {
    state.app_data.lock().expect("app data state poisoned").clone().ok_or_else(|| "Wallis configuration storage is not ready.".to_string())
}

fn open_in_browser(url: &str) -> Result<(), String> {
    #[cfg(windows)]
    let mut opener = { let mut c = std::process::Command::new("rundll32"); c.args(["url.dll,FileProtocolHandler", url]); c };
    #[cfg(not(windows))]
    let mut opener = { let mut c = std::process::Command::new("xdg-open"); c.arg(url); c };
    opener.spawn().map(|_| ()).map_err(|error| format!("Unable to open the browser: {error}"))
}

#[tauri::command]
pub fn codex_signed_in(state: State<'_, AppState>) -> Result<bool, String> {
    let dir = role_dir(&app_data(&state)?, ROLES[0]);
    Ok(std::fs::read_to_string(dir.join("auth.json")).map(|text| codex_auth::signed_in(&text)).unwrap_or(false))
}

/// Opens the ChatGPT sign-in page and waits for the browser to come back, then signs both roles in.
#[tauri::command]
pub async fn codex_sign_in(app: tauri::AppHandle) -> Result<bool, String> {
    let app_data = app.state::<AppState>().app_data.lock().expect("app data state poisoned").clone().ok_or("Wallis configuration storage is not ready.")?;
    tauri::async_runtime::spawn_blocking(move || {
        let pending = codex_auth::begin()?;
        open_in_browser(&pending.url)?;
        let code = codex_auth::wait_for_code(&pending, Duration::from_secs(300))?;
        let credentials = codex_auth::exchange(&pending, &code)?;
        ROLES.iter().try_for_each(|role| codex_auth::store(&role_dir(&app_data, role), &credentials))?;
        Ok(true)
    }).await.map_err(|error| error.to_string())?
}

#[tauri::command]
pub fn codex_sign_out(state: State<'_, AppState>) -> Result<bool, String> {
    let app_data = app_data(&state)?;
    for role in ROLES {
        let path = role_dir(&app_data, role).join("auth.json");
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let Ok(mut root) = serde_json::from_str::<serde_json::Value>(&text) else { continue };
        if root.as_object_mut().and_then(|map| map.remove("openai-codex")).is_some() {
            std::fs::write(&path, root.to_string()).map_err(|error| format!("Unable to update {}: {error}", path.display()))?;
        }
    }
    Ok(false)
}

/// Models, reasoning levels and Fast-mode support for this ChatGPT account, straight from Codex.
#[tauri::command]
pub async fn codex_models(app: tauri::AppHandle) -> Result<Vec<catalog::CodexModel>, String> {
    let app_data = app.state::<AppState>().app_data.lock().expect("app data state poisoned").clone().ok_or("Wallis configuration storage is not ready.")?;
    tauri::async_runtime::spawn_blocking(move || {
        let credentials = codex_auth::fresh(&role_dir(&app_data, ROLES[0]))?;
        // The second role shares the account; keep its copy as current as the first.
        codex_auth::store(&role_dir(&app_data, ROLES[1]), &credentials)?;
        catalog::parse_codex_catalog(&codex_auth::fetch_catalog(&credentials)?)
    }).await.map_err(|error| error.to_string())?
}

pub const ROLES_FILE: &str = "roles.json";

#[derive(Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Roles { pub realtime: wallis_core::role::RoleConfig, pub worker: wallis_core::role::RoleConfig }

#[tauri::command]
pub fn get_roles(state: State<'_, AppState>) -> Result<Roles, String> {
    let path = app_data(&state)?.join(ROLES_FILE);
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).map_err(|error| format!("{} is invalid: {error}", path.display())),
        Err(_) => Ok(Roles::default()),
    }
}

/// Each role is configured on its own; an unset role stays empty rather than failing the save.
#[tauri::command]
pub fn save_roles(roles: Roles, state: State<'_, AppState>) -> Result<Roles, String> {
    for (name, role) in [("Realtime", &roles.realtime), ("Worker", &roles.worker)] {
        if role.is_configured() { role.validate().map_err(|error| format!("{name}: {error}"))?; }
    }
    let dir = app_data(&state)?;
    std::fs::create_dir_all(&dir).map_err(|error| format!("Unable to create the Wallis data folder: {error}"))?;
    std::fs::write(dir.join(ROLES_FILE), serde_json::to_string_pretty(&roles).map_err(|error| error.to_string())?).map_err(|error| format!("Unable to save roles: {error}"))?;
    Ok(roles)
}

pub const PROVIDER_FILE: &str = "provider.json";

/// The OpenAI-compatible endpoint (the ChatGPT sign-in needs no settings). The API key itself lives only in each role's Pi `auth.json`.
#[derive(Default, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Provider { pub base_url: String, pub models: Vec<String>, #[serde(deserialize_with = "efforts_or_empty")] pub efforts: std::collections::BTreeMap<String, Vec<String>>, #[serde(skip_deserializing)] pub has_key: bool }

/// Older builds stored one flat list of levels; that carries no per-model information, so it reads as "none listed".
fn efforts_or_empty<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<std::collections::BTreeMap<String, Vec<String>>, D::Error> {
    let value = <serde_json::Value as serde::Deserialize>::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).unwrap_or_default())
}

fn read_provider(app_data: &Path) -> Provider {
    let mut provider: Provider = std::fs::read_to_string(app_data.join(PROVIDER_FILE)).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default();
    provider.has_key = api_key(app_data).is_some();
    provider
}

fn api_key(app_data: &Path) -> Option<String> {
    let text = std::fs::read_to_string(role_dir(app_data, ROLES[0]).join("auth.json")).ok()?;
    serde_json::from_str::<serde_json::Value>(&text).ok()?.pointer("/custom/key")?.as_str().map(String::from)
}

/// Writes the endpoint into both roles' Pi dirs and remembers it (and its model list) for the UI.
fn apply_provider(app_data: &Path, provider: &Provider, key: &str) -> Result<(), String> {
    ROLES.iter().try_for_each(|role| wallis_core::pi_config::write_provider(&role_dir(app_data, role), &provider.base_url, key, &provider.models))?;
    std::fs::create_dir_all(app_data).map_err(|error| error.to_string())?;
    std::fs::write(app_data.join(PROVIDER_FILE), serde_json::to_string_pretty(provider).map_err(|error| error.to_string())?).map_err(|error| format!("Unable to save the provider: {error}"))
}

#[tauri::command]
pub fn get_provider(state: State<'_, AppState>) -> Result<Provider, String> { Ok(read_provider(&app_data(&state)?)) }

/// Saves the endpoint. An empty `api_key` keeps the stored one (endpoints such as a local server may need none: `clear_key`).
#[tauri::command(rename_all = "camelCase")]
pub fn save_provider(base_url: String, api_key: String, clear_key: bool, state: State<'_, AppState>) -> Result<Provider, String> {
    let base_url = base_url.trim().trim_end_matches('/').to_string();
    if !(base_url.starts_with("https://") || base_url.starts_with("http://")) || base_url.contains(char::is_whitespace) { return Err("Enter the provider's base URL, e.g. https://api.openai.com/v1.".into()); }
    if !api_key.is_empty() && (api_key.len() > 512 || api_key.chars().any(char::is_control)) { return Err("The API key cannot contain control characters.".into()); }
    let app_data = app_data(&state)?;
    let old = read_provider(&app_data);
    let key = if clear_key { String::new() } else if api_key.is_empty() { api_key_or_empty(&app_data) } else { api_key };
    // A different endpoint has different models.
    let models_kept = old.base_url == base_url;
    let mut provider = Provider { base_url, models: if models_kept { old.models } else { vec![] }, efforts: if models_kept { old.efforts } else { Default::default() }, has_key: false };
    apply_provider(&app_data, &provider, &key)?;
    provider.has_key = !key.is_empty();
    Ok(provider)
}

fn api_key_or_empty(app_data: &Path) -> String { api_key(app_data).unwrap_or_default() }

/// "Refresh models": asks the provider what it offers. Model ids feed the dropdowns; reasoning levels per model only if it exposes them.
/// ChatGPT sign-in has its own catalog (`codex_models`).
#[tauri::command]
pub async fn refresh_provider_models(app: tauri::AppHandle) -> Result<Provider, String> {
    let app_data = app.state::<AppState>().app_data.lock().expect("app data state poisoned").clone().ok_or("Wallis configuration storage is not ready.")?;
    tauri::async_runtime::spawn_blocking(move || {
        let mut provider = read_provider(&app_data);
        if provider.base_url.is_empty() { return Err("Save the provider's base URL first.".to_string()); }
        let key = api_key_or_empty(&app_data);
        let found = wallis_core::pi_config::fetch_models(&provider.base_url, &key)?;
        (provider.models, provider.efforts) = (found.models, found.efforts);
        apply_provider(&app_data, &provider, &key)?;
        Ok(provider)
    }).await.map_err(|error| error.to_string())?
}
