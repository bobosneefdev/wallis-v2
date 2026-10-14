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
        catalog::parse_codex_catalog(&codex_auth::fetch_catalog(&credentials, env!("CARGO_PKG_VERSION"))?)
    }).await.map_err(|error| error.to_string())?
}
