mod codex;
use serde::Serialize;
use wallis_core::settings::*;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::{Manager, State};
use wallis_core::provision::{self, STT_MODELS, TTS_VOICES};
use wallis_core::{update, wsl::Session};

const CONFIG_FILE: &str = "config.json";
const TOKEN_FILE: &str = "discord-token.dpapi";
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Default)]
struct AppState { config: Mutex<WallisConfig>, app_data: Mutex<Option<PathBuf>>, running: Mutex<Option<Session>>, progress: Mutex<String> }

fn set_progress(state: &AppState, message: &str) { *state.progress.lock().expect("progress state poisoned") = message.to_string(); }

fn app_data(state: &AppState) -> Result<PathBuf, String> {
    state.app_data.lock().expect("app data state poisoned").clone().ok_or_else(|| "Wallis configuration storage is not ready.".to_string())
}

#[derive(Clone, serde::Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeStatus { state: String, message: String }
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SecretStatus { has_token: bool }
fn config_path(app_data: &Path) -> PathBuf { app_data.join(CONFIG_FILE) }
fn token_path(app_data: &Path) -> PathBuf { app_data.join(TOKEN_FILE) }

#[cfg(target_os = "windows")]
fn protect_token(path: &Path, token: &str) -> Result<(), String> {
    use windows_sys::Win32::Security::Cryptography::{CryptProtectData, CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN};
    use windows_sys::Win32::Foundation::LocalFree;
    let parent = path.parent().ok_or("Unable to determine the Wallis secret folder.")?;
    std::fs::create_dir_all(parent).map_err(|error| format!("Unable to create the Wallis secret folder: {error}"))?;
    let mut input = CRYPT_INTEGER_BLOB { cbData: token.len() as u32, pbData: token.as_ptr() as *mut u8 };
    let mut output = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
    let ok = unsafe { CryptProtectData(&mut input, std::ptr::null(), std::ptr::null(), std::ptr::null_mut(), std::ptr::null(), CRYPTPROTECT_UI_FORBIDDEN, &mut output) };
    if ok == 0 { return Err("Windows could not protect the secret.".into()); }
    let temporary = path.with_extension("dpapi.tmp");
    let result = std::fs::write(&temporary, unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) })
        .map_err(|error| format!("Unable to store the protected secret: {error}"))
        .and_then(|_| {
            let backup = path.with_extension("dpapi.bak");
            let _ = std::fs::remove_file(&backup);
            if path.exists() { std::fs::rename(path, &backup).map_err(|error| format!("Unable to replace the protected secret: {error}"))?; }
            match std::fs::rename(&temporary, path) {
                Ok(()) => { let _ = std::fs::remove_file(backup); Ok(()) }
                Err(error) => {
                    let _ = std::fs::rename(&backup, path);
                    Err(format!("Unable to replace the protected secret: {error}"))
                }
            }
        });
    unsafe { LocalFree(output.pbData as *mut _); }
    let _ = std::fs::remove_file(temporary);
    result
}

#[cfg(target_os = "windows")]
fn unprotect_token(path: &Path) -> Result<String, String> {
    use windows_sys::Win32::Security::Cryptography::{CryptUnprotectData, CRYPT_INTEGER_BLOB};
    use windows_sys::Win32::Foundation::LocalFree;
    let bytes = std::fs::read(path).map_err(|error| format!("Unable to read the protected secret: {error}"))?;
    let mut input = CRYPT_INTEGER_BLOB { cbData: bytes.len() as u32, pbData: bytes.as_ptr() as *mut u8 };
    let mut output = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
    let ok = unsafe { CryptUnprotectData(&mut input, std::ptr::null_mut(), std::ptr::null(), std::ptr::null_mut(), std::ptr::null(), 0, &mut output) };
    if ok == 0 { return Err("Windows could not unlock the stored secret. Save it again.".into()); }
    let result = String::from_utf8(unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec()).map_err(|_| "The stored secret is invalid UTF-8.".to_string());
    unsafe { LocalFree(output.pbData as *mut _); }
    result
}

#[cfg(not(target_os = "windows"))]
fn protect_token(_path: &Path, _token: &str) -> Result<(), String> { Err("Wallis currently requires Windows DPAPI for token storage.".into()) }
#[cfg(not(target_os = "windows"))]
fn unprotect_token(_path: &Path) -> Result<String, String> { Err("Wallis currently requires Windows DPAPI for token storage.".into()) }

fn validate_secret(value: &str, label: &str) -> Result<(), String> {
    if value.is_empty() || value.len() > 512 || value.bytes().any(|byte| byte == b'\r' || byte == b'\n' || byte == 0) { return Err(format!("The {label} must be non-empty and cannot contain newlines.")); }
    Ok(())
}

fn roles(app_data: &Path) -> Result<codex::Roles, String> {
    let text = std::fs::read_to_string(app_data.join(codex::ROLES_FILE)).map_err(|_| "Choose the realtime and worker models in Settings first.".to_string())?;
    let roles: codex::Roles = serde_json::from_str(&text).map_err(|error| format!("roles.json is invalid: {error}"))?;
    for (name, role) in [("Realtime", &roles.realtime), ("Worker", &roles.worker)] {
        if !role.is_configured() { return Err(format!("Choose the {name} model in Settings first.")); }
    }
    Ok(roles)
}

fn reset_runtime(state: &AppState) -> Result<(),String> {
    let mut running = state.running.lock().unwrap();
    if let Some(session) = running.as_mut() { session.request("stop_wallis",serde_json::json!({}))?; }
    running.take();
    Ok(())
}
fn status(state: &str, message: impl Into<String>) -> RuntimeStatus { RuntimeStatus { state: state.into(), message: message.into() } }
fn request(state: &AppState, command: &str, args: serde_json::Value) -> Result<serde_json::Value,String> {
    let mut running = state.running.lock().expect("runtime state poisoned");
    if running.is_none() {
        let data = app_data(state)?;
        let config = state.config.lock().expect("config state poisoned").clone();
        *running = Some(Session::start(&data, config, CURRENT_VERSION, &|s| set_progress(state,s))?);
    }
    running.as_mut().ok_or("Runtime unavailable.")?.request(command,args)
}
fn start_inner(state: &AppState) -> Result<RuntimeStatus,String> {
    let data = app_data(state)?;
    let token = unprotect_token(&token_path(&data))?;
    let mut config = state.config.lock().unwrap().clone();
    validate_voice(&mut config)?; validate_discord_channels(&mut config)?; validate_allowed_users(&config.discord_allowed_users)?;
    roles(&data)?;
    set_progress(state,"Starting the Linux runtime…");
    let result = request(state,"start_wallis",serde_json::json!({"token":token}));
    set_progress(state,"");
    serde_json::from_value(result?).map_err(|e|e.to_string())
}
#[tauri::command]
async fn start_wallis(app: tauri::AppHandle) -> Result<RuntimeStatus,String> { tauri::async_runtime::spawn_blocking(move || start_inner(&app.state::<AppState>())).await.map_err(|e|e.to_string())? }
#[tauri::command]
async fn stop_wallis(app: tauri::AppHandle) -> Result<RuntimeStatus,String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let mut running = state.running.lock().unwrap();
        if let Some(session) = running.as_mut() { session.request("stop_wallis",serde_json::json!({}))?; }
        running.take(); Ok(status("stopped","Wallis is stopped."))
    }).await.map_err(|e|e.to_string())?
}
#[tauri::command]
async fn get_runtime_status(app: tauri::AppHandle) -> RuntimeStatus {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let Ok(mut running) = state.running.try_lock() else { return status("running","The Linux runtime is busy…") };
        match running.as_mut() {
            None => status("stopped","Wallis is stopped."),
            Some(session) => session.request("get_runtime_status",serde_json::json!({})).and_then(|v|serde_json::from_value(v).map_err(|e|e.to_string())).unwrap_or_else(|e|status("unavailable",e)),
        }
    }).await.unwrap_or_else(|e|status("unavailable",e.to_string()))
}
fn poll(state: &AppState, command: &str, args: serde_json::Value, empty: serde_json::Value) -> Result<serde_json::Value,String> {
    let Ok(mut running) = state.running.try_lock() else { return Ok(empty) };
    match running.as_mut() { Some(session) => session.request(command,args), None => Ok(empty) }
}
#[tauri::command]
async fn get_stats(app: tauri::AppHandle) -> Result<serde_json::Value,String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        poll(&state,"get_stats",serde_json::json!({}),serde_json::Value::Null)
    }).await.map_err(|e|e.to_string())?
}
/// Live log lines after `since`; while the runtime is busy or offline, nothing new.
#[tauri::command]
async fn get_log(app: tauri::AppHandle, since: u64) -> Result<serde_json::Value,String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        poll(&state,"get_log",serde_json::json!({"since":since}),serde_json::json!({"next":since,"lines":[]}))
    }).await.map_err(|e|e.to_string())?
}

#[tauri::command]
fn get_config(state: State<'_, AppState>) -> WallisConfig { state.config.lock().expect("config state poisoned").clone() }

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UpdateCheck { current: String, latest: String, available: bool }

#[tauri::command]
async fn check_update() -> Result<UpdateCheck, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let latest = update::latest()?;
        Ok(UpdateCheck { current: CURRENT_VERSION.into(), available: update::newer(CURRENT_VERSION, &latest.version), latest: latest.version })
    }).await.map_err(|error| error.to_string())?
}

/// Downloads the latest installer, then hands over to it: it closes Wallis, upgrades in place and relaunches.
#[tauri::command]
async fn install_update(app: tauri::AppHandle) -> Result<(), String> {
    let dir = app_data(&app.state::<AppState>())?.join("updates");
    tauri::async_runtime::spawn_blocking(move || {
        let latest = update::latest()?;
        if !update::newer(CURRENT_VERSION, &latest.version) { return Err("Wallis is already up to date.".to_string()); }
        let state = app.state::<AppState>();
        set_progress(&state, &format!("Downloading Wallis {}…", latest.version));
        let setup = dir.join("Wallis-setup.exe");
        provision::download_verified(&latest.url, &latest.sha256, &setup)?;
        state.running.lock().expect("running state poisoned").take();
        std::process::Command::new(&setup).args(["/UPDATE", "/P", "/R"]).spawn().map_err(|error| format!("Unable to start the installer: {error}"))?;
        app.exit(0);
        Ok(())
    }).await.map_err(|error| error.to_string())?
}

#[tauri::command]
fn get_progress(state: State<'_, AppState>) -> String { state.progress.lock().expect("progress state poisoned").clone() }

#[tauri::command]
fn get_profile(state: State<'_, AppState>) -> Result<Profile, String> { read_profile(&app_data(&state)?) }

/// Long-running: a one-off generator agent (its own system prompt, not Wallis') writes the identity, stores it, then commits it to Wallis' memory.
#[tauri::command]
async fn generate_wallis(app: tauri::AppHandle, hint: Option<String>) -> Result<Profile, String> {
    let hint: String = hint.unwrap_or_default().chars().take(2000).collect();
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let (app_data, config) = (app_data(&state)?, state.config.lock().expect("config state poisoned").clone());
        // Once created, only Wallis changes the profile (his `update_profile` tool); nobody regenerates it.
        if read_profile(&app_data).is_ok() { return Err("Wallis already has a profile. Only he can change it.".into()); }
        roles(&app_data)?;
        set_progress(&state,"Generating Wallis in Linux…");
        let result = request(&state,"generate_wallis",serde_json::json!({"hint":hint}));
        set_progress(&state,"");
        let profile: Profile = serde_json::from_value(result?).map_err(|e|e.to_string())?;
        let mut config = config;
        config.onboarded = true;
        save_config_file(&config_path(&app_data), &config)?;
        *state.config.lock().expect("config state poisoned") = config;
        set_progress(&state, "");
        Ok(profile)
    }).await.map_err(|error| error.to_string())?
}

/// Erases Wallis: stops him, deletes his profile and long-term memory, and sends him back to onboarding.
#[tauri::command]
async fn lobotomize_wallis(app: tauri::AppHandle) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        reset_runtime(&state)?;
        request(&state,"lobotomize_wallis",serde_json::json!({}))?;
        let app_data = app_data(&state)?;
        let path = app_data.join(PROFILE_FILE);
        if path.exists() { std::fs::remove_file(path).map_err(|e|e.to_string())?; }
        let mut config = state.config.lock().expect("config state poisoned");
        config.onboarded = false;
        save_config_file(&config_path(&app_data), &config)
    }).await.map_err(|e|e.to_string())?
}

#[tauri::command]
async fn save_config(config: WallisConfig, app: tauri::AppHandle) -> Result<WallisConfig, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let mut config = config;
        validate_voice(&mut config)?;
        config.discord_allowed_users = validate_allowed_users(&config.discord_allowed_users)?;
        validate_discord_channels(&mut config)?;
        reset_runtime(&state)?;
        save_config_file(&config_path(&app_data(&state)?), &config)?;
        *state.config.lock().expect("config state poisoned") = config.clone();
        Ok(config)
    }).await.map_err(|e|e.to_string())?
}

#[tauri::command]
fn get_secret_status(state: State<'_, AppState>) -> Result<SecretStatus, String> {
    let path = token_path(&app_data(&state)?);
    Ok(SecretStatus { has_token: path.is_file() && unprotect_token(&path).and_then(|token| validate_secret(&token, "Discord token")).is_ok() })
}

#[tauri::command(rename_all = "camelCase")]
async fn save_discord_token(token: String, app: tauri::AppHandle) -> Result<SecretStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        validate_secret(&token, "Discord token")?;
        reset_runtime(&state)?;
        protect_token(&token_path(&app_data(&state)?), &token)?;
        Ok(SecretStatus { has_token: true })
    }).await.map_err(|e|e.to_string())?
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct VoiceOption { id: String, label: String, megabytes: u32 }

#[tauri::command]
fn get_voice_options() -> (Vec<VoiceOption>, Vec<VoiceOption>) {
    let stt = STT_MODELS.iter().map(|(id, label, _, megabytes)| VoiceOption { id: (*id).to_string(), label: (*label).to_string(), megabytes: *megabytes }).collect();
    (stt, TTS_VOICES.iter().map(|(id, label)| VoiceOption { id: (*id).to_string(), label: (*label).to_string(), megabytes: 0 }).collect())
}

#[tauri::command]
async fn get_jobs(app: tauri::AppHandle) -> Result<serde_json::Value,String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        poll(&state,"get_jobs",serde_json::json!({}),serde_json::json!([]))
    }).await.map_err(|e|e.to_string())?
}
#[tauri::command]
async fn retry_job(id: i64, app: tauri::AppHandle) -> Result<serde_json::Value,String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        request(&state,"retry_job",serde_json::json!({"id":id}))
    }).await.map_err(|e|e.to_string())?
}
#[tauri::command]
async fn get_mods(app: tauri::AppHandle) -> Result<serde_json::Value,String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        request(&state,"get_mods",serde_json::json!({}))
    }).await.map_err(|e|e.to_string())?
}
#[tauri::command]
async fn set_mod_enabled(name: String, enabled: bool, app: tauri::AppHandle) -> Result<serde_json::Value,String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        request(&state,"set_mod_enabled",serde_json::json!({"name":name,"enabled":enabled}))
    }).await.map_err(|e|e.to_string())?
}
#[tauri::command]
async fn delete_mod(name: String, app: tauri::AppHandle) -> Result<serde_json::Value,String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        request(&state,"delete_mod",serde_json::json!({"name":name}))
    }).await.map_err(|e|e.to_string())?
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default().manage(AppState::default()).setup(|app| {
        let app_data = app.path().app_data_dir().map_err(std::io::Error::other)?;
        let config = load_config_file(&config_path(&app_data)).map_err(std::io::Error::other)?;
        let state = app.state::<AppState>();
        *state.config.lock().expect("config state poisoned") = config.clone();
        *state.app_data.lock().expect("app data state poisoned") = Some(app_data);
        // Off the main thread: provisioning may download, and the window should paint first.
        if config.auto_start {
            let handle = app.handle().clone();
            std::thread::spawn(move || { let _ = start_inner(&handle.state::<AppState>()); });
        }
        Ok(())
    }).invoke_handler(tauri::generate_handler![get_runtime_status, get_stats, get_log, start_wallis, stop_wallis, get_config, save_config, get_secret_status, save_discord_token, get_profile, generate_wallis, lobotomize_wallis, get_progress, check_update, install_update, get_voice_options, get_jobs, retry_job, get_mods, set_mod_enabled, delete_mod, codex::codex_signed_in, codex::codex_sign_in, codex::codex_sign_out, codex::codex_models, codex::get_roles, codex::save_roles, codex::get_provider, codex::save_provider, codex::refresh_provider_models]).run(tauri::generate_context!()).expect("error while running Wallis");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_allow_list_and_round_trips_non_secret_config() {
        assert_eq!(validate_allowed_users("123456789012").unwrap(), "123456789012");
        assert_eq!(validate_allowed_users("*").unwrap(), "*");
        assert!(validate_allowed_users("all-users").is_err());
        let path = std::env::temp_dir().join(format!("wallis-config-test-{}.json", std::process::id()));
        let config = WallisConfig { discord_allowed_users: "123456789012".into(), ..WallisConfig::default() };
        save_config_file(&path, &config).unwrap();
        assert_eq!(load_config_file(&path).unwrap().discord_allowed_users, config.discord_allowed_users);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn migrates_old_speech_settings_without_losing_other_preferences() {
        let path = std::env::temp_dir().join(format!("wallis-speech-migration-{}.json", std::process::id()));
        std::fs::write(&path, r#"{"sttModel":"small.en","ttsVoice":"high","ttsSpeed":0.7,"sttThreads":2,"discordAllowedUsers":"*"}"#).unwrap();
        let config = load_config_file(&path).unwrap();
        assert_eq!((config.stt_model.as_str(), config.tts_voice.as_str(), config.tts_speed, config.stt_threads, config.discord_allowed_users.as_str()), ("parakeet-0.6b-v3-int8", "emily", 1.0, 2, "*"));
        std::fs::write(&path, r#"{"sttModel":"parakeet-110m-int8","ttsVoice":"paul","ttsSpeed":0.8,"sttThreads":6,"discordAllowedUsers":"*"}"#).unwrap();
        let config = load_config_file(&path).unwrap();
        assert_eq!((config.stt_model.as_str(), config.tts_voice.as_str(), config.tts_speed, config.stt_threads), ("parakeet-0.6b-v3-int8", "paul", 0.8, 6));
        save_config_file(&path, &config).unwrap();
        assert_eq!(load_config_file(&path).unwrap().stt_model, "parakeet-0.6b-v3-int8");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn validates_channels_and_fills_missing_config_fields() {
        let mut config = WallisConfig::default();
        let partial: WallisConfig = serde_json::from_str(r#"{"discordAllowedUsers":"*"}"#).unwrap();
        assert_eq!((partial.voice_participation.as_str(), partial.participation_cooldown_secs, partial.stt_model.as_str()), ("natural", 5, "parakeet-0.6b-v3-int8"));
        config.discord_write_channels = "123456789012".into();
        config.discord_voice_text_channel = "999999999999".into();
        assert!(validate_discord_channels(&mut config).is_err());
        config.discord_voice_text_channel = "123456789012".into();
        assert!(validate_discord_channels(&mut config).is_ok());
        config.voice_participation = "always".into();
        assert!(validate_discord_channels(&mut config).is_err());
        config.voice_participation = "natural".into();
        config.participation_cooldown_secs = 0;
        assert!(validate_discord_channels(&mut config).is_err());
        config.participation_cooldown_secs = 5;
        config.stt_threads = 0;
        assert!(validate_voice(&mut config).is_err());
    }

    #[test]
    fn rejects_secret_values_that_could_escape_the_token_store() {
        assert!(validate_secret("token\nwith-newline", "Discord token").is_err());
        assert!(validate_secret("", "Discord token").is_err());
        assert!(validate_secret("token-value", "Discord token").is_ok());
    }

    #[test]
    fn the_optional_generation_hint_reaches_the_prompt_only_when_given() {
        assert!(!generation_prompt("  ").contains("Preferences"));
        assert!(generation_prompt("a retired lighthouse keeper").contains("Preferences") && generation_prompt("a retired lighthouse keeper").contains("lighthouse keeper"));
    }

    #[test]
    fn locks_the_first_name_whatever_the_model_writes() {
        assert_eq!(lock_first_name("Margot Vance"), "Wallis Vance");
        assert_eq!(lock_first_name("Margot"), "Wallis");
        assert_eq!(lock_first_name(""), "Wallis");
    }

    #[test]
    fn extracts_one_json_object_from_a_chatty_answer() {
        assert_eq!(extract_json("Sure!\n{\"name\": \"Wallis\", \"nested\": {\"a\": 1}}\nHope that helps"), Some("{\"name\": \"Wallis\", \"nested\": {\"a\": 1}}"));
        assert_eq!(extract_json("no json here"), None);
        assert_eq!(extract_json("{\"unterminated\": true"), None);
    }
}
