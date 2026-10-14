mod codex;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{Manager, State};
use wallis_core::jobs::{Job, Jobs};
use wallis_core::participation::Participation;
use wallis_core::provision::{self, Layout, SpeechServers, STT_MODELS, TTS_VOICES};
use wallis_core::update;
use wallis_discord::bot::{self, Bot, BotConfig};

const FIRST_NAME: &str = "Wallis";
const CONFIG_FILE: &str = "config.json";
const TOKEN_FILE: &str = "discord-token.dpapi";
const PROFILE_FILE: &str = "profile.json";
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// What the bot needs while it runs. Dropping it stops speech servers and the bot; the gateway dies with the runtime.
struct Running { bot: Arc<Mutex<Option<Bot>>>, _servers: SpeechServers, runtime: Option<tokio::runtime::Runtime>, failure: Arc<Mutex<Option<String>>> }

impl Drop for Running {
    fn drop(&mut self) { if let Some(runtime) = self.runtime.take() { runtime.shutdown_background(); } }
}

#[derive(Default)]
struct AppState { config: Mutex<WallisConfig>, app_data: Mutex<Option<PathBuf>>, running: Mutex<Option<Running>>, progress: Mutex<String> }

fn set_progress(state: &AppState, message: &str) { *state.progress.lock().expect("progress state poisoned") = message.to_string(); }

fn app_data(state: &AppState) -> Result<PathBuf, String> {
    state.app_data.lock().expect("app data state poisoned").clone().ok_or_else(|| "Wallis configuration storage is not ready.".to_string())
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
struct WallisConfig {
    discord_application_id: String,
    discord_allowed_users: String,
    discord_read_channels: String,
    discord_write_channels: String,
    discord_voice_text_channel: String,
    voice_participation: String,
    text_participation: String,
    participation_cooldown_secs: u32,
    auto_start: bool,
    onboarded: bool,
    stt_model: String,
    stt_threads: u32,
    tts_voice: String,
    tts_speed: f32,
    silence_ms: u32,
}

impl Default for WallisConfig {
    fn default() -> Self {
        Self { discord_application_id: String::new(), discord_allowed_users: String::new(), discord_read_channels: String::new(), discord_write_channels: String::new(), discord_voice_text_channel: String::new(), voice_participation: "natural".into(), text_participation: "natural".into(), participation_cooldown_secs: 5, auto_start: false, onboarded: false, stt_model: "parakeet-110m-int8".into(), stt_threads: 4, tts_voice: "alba".into(), tts_speed: 1.0, silence_ms: 350 }
    }
}

/// Wallis' own dating-profile-style identity, written once by the realtime agent and kept in `profile.json`.
#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Profile {
    #[serde(default)] name: String,
    #[serde(default)] age: String,
    #[serde(default)] pronouns: String,
    #[serde(default)] location: String,
    #[serde(default)] occupation: String,
    #[serde(default)] tagline: String,
    #[serde(default)] bio: String,
    #[serde(default)] interests: Vec<String>,
    #[serde(default)] looking_for: String,
    #[serde(default)] fun_fact: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeStatus { state: String, message: String }

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SecretStatus { has_token: bool }

fn config_path(app_data: &Path) -> PathBuf { app_data.join(CONFIG_FILE) }
fn token_path(app_data: &Path) -> PathBuf { app_data.join(TOKEN_FILE) }

fn load_config_file(path: &Path) -> Result<WallisConfig, String> {
    if !path.is_file() { return Ok(WallisConfig::default()); }
    let mut config: WallisConfig = serde_json::from_slice(&std::fs::read(path).map_err(|error| format!("Unable to read Wallis configuration: {error}"))?)
        .map_err(|error| format!("Wallis configuration is invalid: {error}"))?;
    if ["tiny.en", "base.en", "small.en"].contains(&config.stt_model.as_str()) { config.stt_model = "parakeet-110m-int8".into(); }
    if ["medium", "high"].contains(&config.tts_voice.as_str()) { config.tts_voice = "alba".into(); config.tts_speed = 1.0; }
    Ok(config)
}

fn save_config_file(path: &Path, config: &WallisConfig) -> Result<(), String> {
    let parent = path.parent().ok_or("Unable to determine the Wallis configuration folder.")?;
    std::fs::create_dir_all(parent).map_err(|error| format!("Unable to create the Wallis configuration folder: {error}"))?;
    let temporary = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(config).map_err(|error| format!("Unable to serialize Wallis configuration: {error}"))?;
    std::fs::write(&temporary, bytes).map_err(|error| format!("Unable to write Wallis configuration: {error}"))?;
    let result = std::fs::copy(&temporary, path).map(|_| ()).map_err(|error| format!("Unable to replace Wallis configuration: {error}"));
    let _ = std::fs::remove_file(temporary);
    result
}

fn validate_allowed_users(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() || !value.split(',').all(|id| id == "*" || (10..=25).contains(&id.len()) && id.bytes().all(|byte| byte.is_ascii_digit())) {
        return Err("Add Discord user IDs or * for anyone to the allow-list.".into());
    }
    Ok(value.to_string())
}

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

/// The first name is never configurable: anything Wallis or a model writes is forced back to it.
fn lock_first_name(value: &str) -> String {
    let cleaned: String = value.chars().map(|character| if character.is_control() { ' ' } else { character }).collect();
    let rest = cleaned.split_whitespace().skip(1).collect::<Vec<_>>().join(" ");
    let name = if rest.is_empty() { FIRST_NAME.to_string() } else { format!("{FIRST_NAME} {rest}") };
    name.chars().take(80).collect()
}

fn sanitize_profile(mut profile: Profile) -> Profile {
    profile.name = lock_first_name(&profile.name);
    profile.interests.truncate(12);
    profile
}

/// Voice knobs come from the GUI, so clamp them to what the runtimes actually accept.
fn validate_voice(config: &mut WallisConfig) -> Result<(), String> {
    if !STT_MODELS.iter().any(|(id, ..)| *id == config.stt_model) { return Err("Choose a supported speech recognition model.".into()); }
    if !TTS_VOICES.iter().any(|(id, ..)| *id == config.tts_voice) { return Err("Choose a supported voice.".into()); }
    if !(1..=64).contains(&config.stt_threads) { return Err("Use between 1 and 64 speech recognition threads.".into()); }
    if !(0.5..=2.0).contains(&config.tts_speed) { return Err("Voice speed must be between 0.5 and 2.0.".into()); }
    if !(100..=2000).contains(&config.silence_ms) { return Err("The silence window must be between 100 and 2000 milliseconds.".into()); }
    Ok(())
}

fn validate_discord_channels(config: &mut WallisConfig) -> Result<(), String> {
    for channels in [&mut config.discord_read_channels, &mut config.discord_write_channels] {
        *channels = if channels.trim().is_empty() { String::new() } else {
            validate_allowed_users(channels).map_err(|_| "Channel lists must contain numeric Discord channel IDs separated by commas.".to_string())?
        };
    }
    config.discord_voice_text_channel = config.discord_voice_text_channel.trim().to_string();
    if !config.discord_voice_text_channel.is_empty() && !config.discord_write_channels.split(',').any(|id| id == config.discord_voice_text_channel) {
        return Err("The voice output channel must be in the write channel list.".into());
    }
    if ![&config.voice_participation, &config.text_participation].iter().all(|mode| matches!(mode.as_str(), "mentions" | "natural")) {
        return Err("Choose a supported participation mode.".into());
    }
    if !(5..=300).contains(&config.participation_cooldown_secs) { return Err("Use 5–300 seconds between unprompted contributions.".into()); }
    Ok(())
}

fn extract_json(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let (mut depth, mut in_string, mut escaped) = (0usize, false, false);
    for (index, byte) in text.bytes().enumerate().skip(start) {
        if in_string {
            match byte { _ if escaped => escaped = false, b'\\' => escaped = true, b'"' => in_string = false, _ => {} }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => { depth -= 1; if depth == 0 { return text.get(start..=index); } }
            _ => {}
        }
    }
    None
}

fn read_profile(app_data: &Path) -> Result<Profile, String> {
    let text = std::fs::read_to_string(app_data.join(PROFILE_FILE)).map_err(|_| "Wallis has no profile yet.".to_string())?;
    serde_json::from_str(&text).map(sanitize_profile).map_err(|_| "Wallis' profile file is not valid JSON.".to_string())
}

/// Who the realtime agent is. Before a profile exists it is just the name.
fn persona(app_data: &Path) -> String {
    let Ok(p) = read_profile(app_data) else { return format!("You are {FIRST_NAME}.") };
    format!("# {name}\n\nYou are {name}. This is who you are, not a role you play.\n\n- Age: {age}\n- Pronouns: {pronouns}\n- Lives in: {location}\n- Does: {occupation}\n- In one line: {tagline}\n- Into: {interests}\n- Looking for: {looking_for}\n- Fun fact: {fun_fact}\n\n{bio}\n\nYour first name is always {first}.",
        name = p.name, age = p.age, pronouns = p.pronouns, location = p.location, occupation = p.occupation, tagline = p.tagline, interests = p.interests.join(", "), looking_for = p.looking_for, fun_fact = p.fun_fact, bio = p.bio, first = FIRST_NAME)
}

fn profile_summary(app_data: &Path) -> String {
    match read_profile(app_data) {
        Ok(p) => format!("**{}**{}\n{}\n{}", p.name, if p.tagline.is_empty() { String::new() } else { format!(" — {}", p.tagline) }, p.bio, p.interests.join(" · ")).chars().take(1900).collect(),
        Err(_) => format!("{FIRST_NAME} has no profile yet."),
    }
}

fn generation_prompt(hint: &str) -> String {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|elapsed| elapsed.as_nanos()).unwrap_or_default();
    format!(
        "Invent yourself. You are a fictional person whose first name is always {FIRST_NAME}; pick your own surname, age, home, work and personality. Randomness seed {seed:x} — let it push you somewhere unexpected, not the safest choice.\n\nReply with one JSON object and nothing else, using exactly these keys:\n{{\"name\": \"{FIRST_NAME} <surname>\", \"age\": \"27\", \"pronouns\": \"he/him\", \"location\": \"city, country\", \"occupation\": \"what you do\", \"tagline\": \"one line, under 90 characters\", \"bio\": \"2-4 sentences in your own voice\", \"interests\": [\"5 to 8 short specifics\"], \"lookingFor\": \"what you want from the people you talk to\", \"funFact\": \"one surprising detail\"}}\n\nWrite it like a dating profile: specific, warm, a little strange. No markdown, no code fences, no commentary.{hint}",
        hint = if hint.trim().is_empty() { String::new() } else { format!("\n\nThe person setting you up asked for this baseline. Build the profile around it where it fits and stay yourself otherwise:\n{}", hint.trim()) },
        seed = nanos ^ u128::from(std::process::id()).wrapping_mul(0x9E37_79B9_7F4A_7C15),
    )
}

fn roles(app_data: &Path) -> Result<codex::Roles, String> {
    let text = std::fs::read_to_string(app_data.join(codex::ROLES_FILE)).map_err(|_| "Choose the realtime and worker models in Settings first.".to_string())?;
    let roles: codex::Roles = serde_json::from_str(&text).map_err(|error| format!("roles.json is invalid: {error}"))?;
    for (name, role) in [("Realtime", &roles.realtime), ("Worker", &roles.worker)] {
        if !role.is_configured() { return Err(format!("Choose the {name} model in Settings first.")); }
    }
    Ok(roles)
}

fn bot_config(app_data: &Path, config: &WallisConfig, layout: &Layout) -> Result<BotConfig, String> {
    let roles = roles(app_data)?;
    let list = |value: &str| value.split(',').filter(|id| !id.is_empty()).map(String::from).collect::<Vec<_>>();
    let mode = |value: &str| if value == "mentions" { Participation::Mentions } else { Participation::Natural };
    Ok(BotConfig {
        pi_program: layout.pi().to_string_lossy().into(), worker_program: layout.pi().to_string_lossy().into(), data_dir: app_data.to_path_buf(),
        persona: persona(app_data), realtime: roles.realtime, worker: roles.worker, write_channels: list(&config.discord_write_channels),
        python: layout.python().to_string_lossy().into(), voice_participation: mode(&config.voice_participation), text_participation: mode(&config.text_participation),
        cooldown: Duration::from_secs(config.participation_cooldown_secs.into()), speech_speed: config.tts_speed,
        sandbox_read: vec![layout.tools()], shell: layout.shell(),
    })
}

/// Pi runs offline, so each role's packages are fetched here, once per start, into that role's own Pi dir.
fn install_packages(layout: &Layout, app_data: &Path) -> Result<(), String> {
    let roles = roles(app_data)?;
    for (name, role) in [("realtime", &roles.realtime), ("worker", &roles.worker)] {
        wallis_core::pi_config::install_packages(&layout.pi().to_string_lossy(), &codex::role_dir(app_data, name), &role.packages)?;
    }
    Ok(())
}

fn status(state: &str, message: impl Into<String>) -> RuntimeStatus { RuntimeStatus { state: state.into(), message: message.into() } }

fn start_inner(state: &AppState) -> Result<RuntimeStatus, String> {
    if state.running.lock().expect("running state poisoned").is_some() { return Ok(status("running", "Wallis is already running.")); }
    let (app_data, mut config) = (app_data(state)?, state.config.lock().expect("config state poisoned").clone());
    if !config.onboarded { return Err("Finish setting Wallis up before starting him.".into()); }
    let token = unprotect_token(&token_path(&app_data)).and_then(|token| validate_secret(&token, "Discord token").map(|_| token)).map_err(|_| "Save the Discord token first.".to_string())?;
    validate_discord_channels(&mut config)?;
    let layout = Layout(app_data.clone());
    let bot_config = bot_config(&app_data, &config, &layout)?;
    provision::ensure_all(&layout, &config.stt_model, &config.tts_voice, &|message| set_progress(state, message))?;
    install_packages(&layout, &app_data)?;
    set_progress(state, "Starting speech…");
    let servers = SpeechServers::start(&layout, &config.stt_model, config.stt_threads, &config.tts_voice)?;
    set_progress(state, "");
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().map_err(|error| error.to_string())?;
    let (bot, failure) = (Arc::new(Mutex::new(None::<Bot>)), Arc::new(Mutex::new(None::<String>)));
    let (on_ready, on_event, on_end) = (bot.clone(), bot.clone(), failure.clone());
    let settings = wallis_discord::Settings { token, allowed_users: config.discord_allowed_users.split(',').map(String::from).collect(), read_channels: config.discord_read_channels.split(',').filter(|id| !id.is_empty()).map(String::from).collect(), profile: profile_summary(&app_data) };
    let startup_failure = failure.clone();
    runtime.spawn(async move {
        let ready = move |out| {
            // bot::start talks to Discord with blocking calls, which must not run on the gateway's async task.
            std::thread::spawn(move || match bot::start(bot_config, out) {
                Ok(started) => *on_ready.lock().expect("bot state poisoned") = Some(started),
                Err(error) => *startup_failure.lock().expect("failure state poisoned") = Some(error),
            });
        };
        let events = move |event| { if let Some(bot) = on_event.lock().expect("bot state poisoned").as_ref() { let _ = bot.sender.send(event); } };
        if let Err(error) = wallis_discord::run(settings, ready, events).await { *on_end.lock().expect("failure state poisoned") = Some(error); }
    });
    *state.running.lock().expect("running state poisoned") = Some(Running { bot, _servers: servers, runtime: Some(runtime), failure });
    Ok(status("running", "Wallis is online. Use /join in Discord to bring him into a voice channel."))
}

#[tauri::command]
async fn start_wallis(app: tauri::AppHandle) -> Result<RuntimeStatus, String> {
    tauri::async_runtime::spawn_blocking(move || start_inner(&app.state::<AppState>())).await.map_err(|error| error.to_string())?
}

#[tauri::command]
fn stop_wallis(state: State<'_, AppState>) -> RuntimeStatus {
    state.running.lock().expect("running state poisoned").take();
    status("stopped", "Wallis is stopped.")
}

#[tauri::command]
fn get_runtime_status(state: State<'_, AppState>) -> RuntimeStatus {
    match state.running.lock().expect("running state poisoned").as_ref() {
        None => status("stopped", "Wallis is stopped."),
        Some(running) => match running.failure.lock().expect("failure state poisoned").clone() {
            Some(error) => status("unavailable", error),
            None if running.bot.lock().expect("bot state poisoned").is_some() => status("running", "Wallis is online."),
            None => status("running", "Wallis is connecting to Discord…"),
        },
    }
}

#[tauri::command]
fn get_stats(state: State<'_, AppState>) -> Option<wallis_core::stats::Snapshot> {
    let running = state.running.lock().expect("running state poisoned");
    let bot = running.as_ref()?.bot.lock().expect("bot state poisoned");
    let snapshot = bot.as_ref().map(|bot| bot.stats.snapshot());
    snapshot
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
        std::process::Command::new(&setup).args(["/P", "/R"]).spawn().map_err(|error| format!("Unable to start the installer: {error}"))?;
        app.exit(0);
        Ok(())
    }).await.map_err(|error| error.to_string())?
}

#[tauri::command]
fn get_progress(state: State<'_, AppState>) -> String { state.progress.lock().expect("progress state poisoned").clone() }

#[tauri::command]
fn get_profile(state: State<'_, AppState>) -> Result<Profile, String> { read_profile(&app_data(&state)?) }

/// Long-running: has the realtime agent invent itself, stores that identity, then has it commit the identity to memory.
#[tauri::command]
async fn generate_wallis(app: tauri::AppHandle, hint: Option<String>) -> Result<Profile, String> {
    let hint: String = hint.unwrap_or_default().chars().take(2000).collect();
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let (app_data, config) = (app_data(&state)?, state.config.lock().expect("config state poisoned").clone());
        // Once created, only Wallis changes the profile (his `update_profile` tool); nobody regenerates it.
        if read_profile(&app_data).is_ok() { return Err("Wallis already has a profile. Only he can change it.".into()); }
        let layout = Layout(app_data.clone());
        provision::ensure_all(&layout, &config.stt_model, &config.tts_voice, &|message| set_progress(&state, message))?;
        install_packages(&layout, &app_data)?;
        set_progress(&state, "Generating Wallis…");
        let mut agent = bot::spawn_realtime(&bot_config(&app_data, &config, &layout)?)?;
        let answer = agent.turn(&generation_prompt(&hint), Duration::from_secs(120), &mut |_| "Not available while inventing yourself.".into(), &mut |_| {})?;
        let json = extract_json(&answer).ok_or_else(|| format!("The model did not return a profile. It said: {}", answer.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(200).collect::<String>()))?;
        let profile = sanitize_profile(serde_json::from_str(json).map_err(|error| format!("The model returned a profile Wallis could not read: {error}"))?);
        let stored = serde_json::to_string_pretty(&profile).map_err(|error| error.to_string())?;
        std::fs::write(app_data.join(PROFILE_FILE), &stored).map_err(|error| format!("Unable to save the profile: {error}"))?;
        // A fresh agent now carries the saved persona, so it remembers itself as it will in conversation.
        let mut agent = bot::spawn_realtime(&bot_config(&app_data, &config, &layout)?)?;
        agent.turn(&format!("This is who you are. Commit every detail to your long-term memory as durable facts about yourself, then reply with the word done.\n\n{stored}"), Duration::from_secs(120), &mut |_| "Not available right now.".into(), &mut |_| {})?;
        let mut config = config;
        config.onboarded = true;
        save_config_file(&config_path(&app_data), &config)?;
        *state.config.lock().expect("config state poisoned") = config;
        set_progress(&state, "");
        Ok(profile)
    }).await.map_err(|error| error.to_string())?
}

#[tauri::command]
fn save_config(config: WallisConfig, state: State<'_, AppState>) -> Result<WallisConfig, String> {
    let mut config = config;
    validate_voice(&mut config)?;
    config.discord_allowed_users = validate_allowed_users(&config.discord_allowed_users)?;
    validate_discord_channels(&mut config)?;
    save_config_file(&config_path(&app_data(&state)?), &config)?;
    *state.config.lock().expect("config state poisoned") = config.clone();
    Ok(config)
}

#[tauri::command]
fn get_secret_status(state: State<'_, AppState>) -> Result<SecretStatus, String> {
    let path = token_path(&app_data(&state)?);
    Ok(SecretStatus { has_token: path.is_file() && unprotect_token(&path).and_then(|token| validate_secret(&token, "Discord token")).is_ok() })
}

#[tauri::command(rename_all = "camelCase")]
fn save_discord_token(token: String, state: State<'_, AppState>) -> Result<SecretStatus, String> {
    validate_secret(&token, "Discord token")?;
    protect_token(&token_path(&app_data(&state)?), &token)?;
    Ok(SecretStatus { has_token: true })
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct VoiceOption { id: String, label: String, megabytes: u32 }

#[tauri::command]
fn get_voice_options() -> (Vec<VoiceOption>, Vec<VoiceOption>) {
    let map = |list: &[(&str, &str, &str, u32)]| list.iter().map(|(id, label, _, megabytes)| VoiceOption { id: (*id).to_string(), label: (*label).to_string(), megabytes: *megabytes }).collect();
    (map(&STT_MODELS), map(&TTS_VOICES))
}

/// Delegated jobs, newest state included, so the UI can offer a manual retry on failed ones.
#[tauri::command]
fn get_jobs(state: State<'_, AppState>) -> Result<Vec<Job>, String> {
    let path = app_data(&state)?.join("jobs.sqlite");
    if !path.is_file() { return Ok(Vec::new()); }
    Jobs::open(&path.to_string_lossy())?.with_status(&["queued", "running", "done", "failed"])
}

#[tauri::command]
fn retry_job(id: i64, state: State<'_, AppState>) -> Result<bool, String> {
    Jobs::open(&app_data(&state)?.join("jobs.sqlite").to_string_lossy())?.retry(id)
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
    }).invoke_handler(tauri::generate_handler![get_runtime_status, get_stats, start_wallis, stop_wallis, get_config, save_config, get_secret_status, save_discord_token, get_profile, generate_wallis, get_progress, check_update, install_update, get_voice_options, get_jobs, retry_job, codex::codex_signed_in, codex::codex_sign_in, codex::codex_sign_out, codex::codex_models, codex::get_roles, codex::save_roles, codex::get_provider, codex::save_provider, codex::refresh_provider_models]).run(tauri::generate_context!()).expect("error while running Wallis");
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
        assert_eq!((config.stt_model.as_str(), config.tts_voice.as_str(), config.tts_speed, config.stt_threads, config.discord_allowed_users.as_str()), ("parakeet-110m-int8", "alba", 1.0, 2, "*"));
        save_config_file(&path, &config).unwrap();
        assert_eq!(load_config_file(&path).unwrap().tts_voice, "alba");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn validates_channels_and_fills_missing_config_fields() {
        let mut config = WallisConfig::default();
        let partial: WallisConfig = serde_json::from_str(r#"{"discordAllowedUsers":"*"}"#).unwrap();
        assert_eq!((partial.voice_participation.as_str(), partial.participation_cooldown_secs, partial.stt_model.as_str()), ("natural", 5, "parakeet-110m-int8"));
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
        assert!(!generation_prompt("  ").contains("baseline"));
        assert!(generation_prompt("a retired lighthouse keeper").contains("baseline") && generation_prompt("a retired lighthouse keeper").contains("lighthouse keeper"));
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
