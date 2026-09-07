use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use tauri::State;

#[derive(Default)]
struct AppState {
    config: Mutex<WallisConfig>,
    running: Mutex<bool>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct WallisConfig {
    discord_application_id: String,
    persona_name: String,
    memory_provider: String,
    auto_start: bool,
}

impl Default for WallisConfig {
    fn default() -> Self {
        Self { discord_application_id: String::new(), persona_name: "Wallis".into(), memory_provider: "mnemosyne".into(), auto_start: false }
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SetupStatus { wsl2_available: bool, message: String }

#[derive(Clone, Serialize)]
struct RuntimeStatus { state: String, message: String }

#[tauri::command]
fn get_setup_status() -> SetupStatus {
    #[cfg(target_os = "windows")]
    {
        return match std::process::Command::new("wsl.exe").arg("--status").output() {
            Ok(output) if output.status.success() => SetupStatus { wsl2_available: true, message: "WSL2 is available.".into() },
            _ => SetupStatus { wsl2_available: false, message: "WSL2 is unavailable. Install or enable WSL2, then restart Wallis.".into() },
        };
    }
    #[allow(unreachable_code)]
    SetupStatus { wsl2_available: false, message: "Wallis currently requires Windows with WSL2.".into() }
}

#[tauri::command]
fn get_runtime_status(state: State<'_, AppState>) -> RuntimeStatus {
    let running = *state.running.lock().expect("runtime state poisoned");
    RuntimeStatus { state: if running { "running" } else { "stopped" }.into(), message: if running { "Hermes is running." } else { "Hermes is stopped." }.into() }
}

#[tauri::command]
fn start_hermes() -> Result<RuntimeStatus, String> {
    Err("Hermes lifecycle is not configured until a Wallis distribution is imported.".into())
}

#[tauri::command]
fn stop_hermes(state: State<'_, AppState>) -> RuntimeStatus {
    *state.running.lock().expect("runtime state poisoned") = false;
    RuntimeStatus { state: "stopped".into(), message: "Hermes is stopped.".into() }
}

#[tauri::command]
fn get_config(state: State<'_, AppState>) -> WallisConfig {
    state.config.lock().expect("config state poisoned").clone()
}

#[tauri::command]
fn save_config(config: WallisConfig, state: State<'_, AppState>) -> Result<WallisConfig, String> {
    if !config.persona_name.starts_with("Wallis") { return Err("Persona name must start with Wallis.".into()); }
    *state.config.lock().expect("config state poisoned") = config.clone();
    Ok(config)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![get_setup_status, get_runtime_status, start_hermes, stop_hermes, get_config, save_config])
        .run(tauri::generate_context!())
        .expect("error while running Wallis");
}
