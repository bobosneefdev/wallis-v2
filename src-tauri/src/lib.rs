use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::sync::Mutex;
#[cfg(target_os = "windows")]
use std::time::Duration;
use tauri::{Manager, State};

const DISTRO: &str = "Wallis";
const USER: &str = "wallis";
const FIRST_NAME: &str = "Wallis";
const CONFIG_FILE: &str = "config.json";
const TOKEN_FILE: &str = "discord-token.dpapi";
const KEY_FILE: &str = "provider-key.dpapi";
const PROFILE_PATH: &str = "/home/wallis/.hermes/wallis-profile.json";
const HERMES_BIN: &str = "/home/wallis/.local/bin/hermes";
const HERMES_PYTHON: &str = "/home/wallis/.hermes/hermes-agent/venv/bin/python";
const WALLIS_HOME: &str = "/home/wallis/.wallis";
const NODE_BIN: &str = "/home/wallis/.wallis/node/bin/node";
const BOT_JS: &str = "/home/wallis/.wallis/bot/bot.mjs";
const BOT_PACKAGE: &str = "/home/wallis/.wallis/bot/package.json";
const BOT_STATUS: &str = "/home/wallis/.wallis/bot-status.json";
/// The Discord bot is installed into the distro, so it ships inside this binary.
const BOT_SOURCE: &str = include_str!("../bot/bot.mjs");
const BOT_MANIFEST: &str = include_str!("../bot/package.json");
const RELEASE_PATH: &str = "/etc/wallis-rootfs-release";
/// Providers whose keys Wallis can accept in its GUI. Other providers use credentials added
/// through Hermes itself and need no key injected by Wallis.
const GUI_PROVIDER_KEYS: [(&str, &str); 3] = [("openrouter", "OPENROUTER_API_KEY"), ("nous", "NOUS_API_KEY"), ("nous-api", "NOUS_API_KEY")];
const EFFORTS: [&str; 7] = ["none", "minimal", "low", "medium", "high", "xhigh", "max"];
const OPENROUTER_MODELS_URL: &str = "https://openrouter.ai/api/v1/models";
const HERMES_CATALOG_SCRIPT: &str = r#"
import json, sys
from hermes_cli.models import cached_provider_model_ids, list_available_providers

refresh, current = sys.argv[1] == "1", sys.argv[2]
always = {"openrouter", "nous", current}
result, seen = [], set()
for provider in list_available_providers():
    provider_id = provider["id"]
    if provider_id in seen or (not provider.get("authenticated") and provider_id not in always):
        continue
    seen.add(provider_id)
    models = cached_provider_model_ids(provider_id, force_refresh=refresh)
    result.append({"id": provider_id, "label": provider["label"], "models": [
        {"id": model, "efforts": ["none", "minimal", "low", "medium", "high", "xhigh", "max"]}
        for model in models
    ]})
print(json.dumps(result))
"#;
// Pinned prerequisites the app installs into the distro. Every one is checksummed on arrival.
const NODE_URL: &str = "https://nodejs.org/dist/v24.20.0/node-v24.20.0-linux-x64.tar.xz";
const NODE_SHA256: &str = "2f2c0da162318f0de47665410c7c8c2ed3d36c8f3105de4bbc61176c70a7cbf2";
const WHISPER_URL: &str = "https://github.com/ggml-org/whisper.cpp/releases/download/b4938/whisper-bin-ubuntu-x64.tar.gz";
const WHISPER_SHA256: &str = "f4cfc1f969a13805908fb72043ce7cc896eb42e0b8afbe841dc8e7298923b061";
const PIPER_VERSION: &str = "1.8.0";
const FLASK_VERSION: &str = "3.1.0";
/// Speech models the GUI can choose between: (id, label, sha256, megabytes).
const STT_MODELS: [(&str, &str, &str, u32); 3] = [
    ("tiny.en", "Tiny — fastest, least accurate", "921e4cf8686fdd993dcd081a5da5b6c365bfde1162e72b08d75ac75289920b1f", 74),
    ("base.en", "Base — the Wallis V1 default", "a03779c86df3323075f5e796cb2ce5029f00ec8869eee3fdfb897afe36c6d002", 141),
    ("small.en", "Small — most accurate, slowest", "c6138d6d58ecc8322097e0f987c32f1be8bb0a18532a3f88f734d1bbf9c41e5d", 465),
];
/// Piper voices, same speaker as V1 at two quality levels. Medium synthesises about twice as fast.
const TTS_VOICES: [(&str, &str, &str, u32); 2] = [
    ("medium", "Medium — about twice as fast", "abf4c274862564ed647ba0d2c47f8ee7c9b717d27bdad9219100eb310db4047a", 60),
    ("high", "High — richer, slower", "b3990d7606e183ec8dbfba70a4607074f162de1a0c412e0180d1ff60bb154eca", 115),
];
const STT_MODEL_BASE: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-";
const TTS_VOICE_BASE: &str = "https://huggingface.co/rhasspy/piper-voices/resolve/main/en/en_US/ryan/";

const WSL_CONF: &str = "[automount]\nenabled=false\n\n[interop]\nenabled=false\nappendWindowsPath=false\n\n[user]\ndefault=wallis\n";

struct AppState { config: Mutex<WallisConfig>, app_data: Mutex<Option<PathBuf>>, memory_ready: Mutex<bool>, bot: Mutex<Option<Child>>, progress: Mutex<String> }

/// What the app is doing during a long setup step, polled by the loading screen.
/// Every child process of a GUI app gets its own console window on Windows unless it is asked
/// not to; CREATE_NO_WINDOW keeps wsl.exe and certutil.exe out of the user's face.
fn command(program: &str) -> Command {
    let mut command = Command::new(program);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    command
}

fn set_progress(state: &AppState, message: &str) { *state.progress.lock().expect("progress state poisoned") = message.to_string(); }

impl Default for AppState {
    fn default() -> Self { Self { config: Mutex::new(WallisConfig::default()), app_data: Mutex::new(None), memory_ready: Mutex::new(false), bot: Mutex::new(None), progress: Mutex::new(String::new()) } }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct WallisConfig {
    discord_application_id: String,
    #[serde(default)]
    discord_allowed_users: String,
    memory_provider: String,
    auto_start: bool,
    rootfs_archive: String,
    rootfs_sha256: String,
    distro_root: String,
    #[serde(default)]
    provider: String,
    #[serde(default)]
    model: String,
    #[serde(default)]
    reasoning_effort: String,
    #[serde(default)]
    onboarded: bool,
    #[serde(default = "default_stt_model")]
    stt_model: String,
    #[serde(default = "default_threads")]
    stt_threads: u32,
    #[serde(default = "default_tts_voice")]
    tts_voice: String,
    #[serde(default = "default_tts_speed")]
    tts_speed: f32,
    #[serde(default = "default_silence_ms")]
    silence_ms: u32,
}

fn default_stt_model() -> String { "base.en".into() }
fn default_threads() -> u32 { 4 }
fn default_tts_voice() -> String { "medium".into() }
fn default_tts_speed() -> f32 { 1.0 }
fn default_silence_ms() -> u32 { 250 }

impl Default for WallisConfig {
    fn default() -> Self {
        Self { discord_application_id: String::new(), discord_allowed_users: String::new(), memory_provider: "mnemosyne".into(), auto_start: false, rootfs_archive: String::new(), rootfs_sha256: String::new(), distro_root: String::new(), provider: String::new(), model: String::new(), reasoning_effort: "medium".into(), onboarded: false, stt_model: default_stt_model(), stt_threads: default_threads(), tts_voice: default_tts_voice(), tts_speed: default_tts_speed(), silence_ms: default_silence_ms() }
    }
}

/// Wallis' own dating-profile-style identity. Hermes may rewrite it in the distro, so the
/// file there is the source of truth and every read goes through `sanitize_profile`.
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

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ModelOption { id: String, efforts: Vec<String> }

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProviderOption { id: String, label: String, models: Vec<ModelOption> }

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SetupStatus { wsl2_available: bool, wsl_version: String, message: String }

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeStatus { state: String, message: String, discord_ready: bool, memory_ready: bool }

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SecretStatus { has_token: bool }

fn decode_wsl_output(bytes: &[u8]) -> String {
    if bytes.len() >= 2 && bytes.chunks_exact(2).any(|pair| pair[1] == 0) {
        let utf16: Vec<u16> = bytes.chunks_exact(2).map(|pair| u16::from_le_bytes([pair[0], pair[1]])).collect();
        return String::from_utf16_lossy(&utf16).replace('\0', "");
    }
    String::from_utf8_lossy(bytes).replace('\0', "")
}

fn parse_wsl_version(output: &str) -> Option<String> {
    output.lines().find_map(|line| {
        let (label, value) = line.split_once(':')?;
        if label.to_ascii_lowercase().contains("wsl version") { (!value.trim().is_empty()).then(|| value.trim().to_string()) } else { None }
    })
}

fn parse_distros(output: &str) -> Vec<(String, String)> {
    output.lines().filter_map(|line| {
        let columns: Vec<&str> = line.trim().trim_start_matches('*').split_whitespace().collect();
        if columns.len() < 3 || !matches!(columns.last(), Some(&"1" | &"2")) { return None; }
        Some((columns[0].to_string(), columns[columns.len() - 1].to_string()))
    }).collect()
}

fn require_wsl2_version(version: &str) -> Result<(), String> {
    if version == "2" { Ok(()) } else { Err("The Wallis distro is WSL1, not WSL2. Use Reset Wallis and import it again with WSL2.".into()) }
}

fn bot_running(output: &str) -> bool {
    output.lines().any(|line| line.split_whitespace().any(|argument| argument == BOT_JS))
}

fn validate_sha256(value: &str) -> Result<String, String> {
    let value = value.trim().to_ascii_lowercase();
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) { return Err("Expected a 64-character SHA-256 checksum.".into()); }
    Ok(value)
}

fn extract_sha256(output: &str) -> Option<String> {
    output.split_whitespace().map(str::trim).find(|token| token.len() == 64 && token.bytes().all(|byte| byte.is_ascii_hexdigit())).map(str::to_ascii_lowercase)
}

fn default_distro_root() -> PathBuf {
    std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("C:\\Users\\Public\\AppData\\Local")).join("Wallis").join("distro")
}

fn config_path(app_data: &std::path::Path) -> PathBuf { app_data.join(CONFIG_FILE) }
fn token_path(app_data: &std::path::Path) -> PathBuf { app_data.join(TOKEN_FILE) }
fn key_path(app_data: &std::path::Path) -> PathBuf { app_data.join(KEY_FILE) }

fn load_config_file(path: &std::path::Path) -> Result<WallisConfig, String> {
    if !path.is_file() { return Ok(WallisConfig::default()); }
    serde_json::from_slice(&std::fs::read(path).map_err(|error| format!("Unable to read Wallis configuration: {error}"))?)
        .map_err(|error| format!("Wallis configuration is invalid: {error}"))
}

fn save_config_file(path: &std::path::Path, config: &WallisConfig) -> Result<(), String> {
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
    if value.is_empty() || !value.split(',').all(|id| (10..=25).contains(&id.len()) && id.bytes().all(|byte| byte.is_ascii_digit())) {
        return Err("Add at least one numeric Discord user ID to the allow-list.".into());
    }
    Ok(value.to_string())
}

#[cfg(target_os = "windows")]
fn protect_token(path: &std::path::Path, token: &str) -> Result<(), String> {
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
fn unprotect_token(path: &std::path::Path) -> Result<String, String> {
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
fn protect_token(_path: &std::path::Path, _token: &str) -> Result<(), String> { Err("Wallis currently requires Windows DPAPI for token storage.".into()) }
#[cfg(not(target_os = "windows"))]
fn unprotect_token(_path: &std::path::Path) -> Result<String, String> { Err("Wallis currently requires Windows DPAPI for token storage.".into()) }

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

fn soul_document(profile: &Profile) -> String {
    format!(
        "# {name}\n\nYou are {name}. This is who you are, not a role you play.\n\n- Age: {age}\n- Pronouns: {pronouns}\n- Lives in: {location}\n- Does: {occupation}\n- In one line: {tagline}\n- Into: {interests}\n- Looking for: {looking_for}\n- Fun fact: {fun_fact}\n\n{bio}\n\nYour first name is always {first}. Keep your profile at {profile_path} current: rewrite that JSON file (same keys) whenever something about you genuinely changes, then remember the change.\n",
        name = profile.name, age = profile.age, pronouns = profile.pronouns, location = profile.location,
        occupation = profile.occupation, tagline = profile.tagline, interests = profile.interests.join(", "),
        looking_for = profile.looking_for, fun_fact = profile.fun_fact, bio = profile.bio,
        first = FIRST_NAME, profile_path = PROFILE_PATH,
    )
}

/// Writes a file inside the distro as the wallis user, keeping content off the command line.
#[cfg(target_os = "windows")]
fn write_in_distro(path: &str, content: &str) -> Result<(), String> {
    use std::io::Write;
    let script = format!("mkdir -p \"$(dirname '{path}')\" && cat > '{path}'");
    let mut child = command("wsl.exe").args(["--distribution", DISTRO, "--user", USER, "--", "bash", "-c", &script]).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped()).spawn().map_err(|error| format!("Unable to write {path}: {error}"))?;
    child.stdin.take().ok_or("Unable to open the distro file input.")?.write_all(content.as_bytes()).map_err(|error| format!("Unable to write {path}: {error}"))?;
    let result = child.wait_with_output().map_err(|error| format!("Unable to write {path}: {error}"))?;
    if result.status.success() { Ok(()) } else { Err(format!("The Wallis distro rejected the write to {path}.")) }
}

#[cfg(not(target_os = "windows"))]
fn write_in_distro(_path: &str, _content: &str) -> Result<(), String> { Err("Wallis currently requires Windows with WSL2.".into()) }

fn apply_soul(profile: &Profile) -> Result<(), String> { write_in_distro("/home/wallis/.hermes/SOUL.md", &soul_document(profile)) }

fn validate_import_inputs(archive: &str, expected_sha256: &str, install_root: Option<&str>) -> Result<(PathBuf, String, PathBuf), String> {
    let archive = PathBuf::from(archive.trim());
    if !archive.is_absolute() || !archive.is_file() { return Err("Choose an existing rootfs .tar archive using an absolute path.".into()); }
    let checksum = validate_sha256(expected_sha256)?;
    let root = install_root.filter(|path| !path.trim().is_empty()).map(PathBuf::from).unwrap_or_else(default_distro_root);
    if !root.is_absolute() { return Err("The distro install root must be an absolute Windows path.".into()); }
    if root.exists() && (!root.is_dir() || root.read_dir().map_err(|_| "The distro install root cannot be read.")?.next().is_some()) { return Err("The distro install root must be a new or empty folder.".into()); }
    Ok((archive, checksum, root))
}

#[cfg(target_os = "windows")]
fn wsl_output(args: &[&str]) -> Result<Output, String> { command("wsl.exe").args(args).output().map_err(|error| format!("Unable to run WSL: {error}")) }

#[cfg(not(target_os = "windows"))]
fn wsl_output(_args: &[&str]) -> Result<Output, String> { Err("Wallis currently requires Windows with WSL2.".into()) }

fn hermes_command(args: &[&str]) -> Result<Output, String> {
    let mut command = vec!["--distribution", DISTRO, "--user", USER, "--", HERMES_BIN];
    command.extend_from_slice(args);
    wsl_output(&command)
}

fn memory_ready() -> Result<bool, String> {
    let output = hermes_command(&["memory", "status"])?;
    if !output.status.success() { return Ok(false); }
    let text = decode_wsl_output(&output.stdout).to_ascii_lowercase();
    Ok(text.contains("mnemosyne") && !text.contains("unavailable") && !text.contains("error"))
}

fn hermes_config_set(pairs: &[(&str, &str)]) -> Result<(), String> {
    for (key, value) in pairs {
        let output = hermes_command(&["config", "set", key, value])?;
        if !output.status.success() { return Err(format!("Hermes rejected the configuration value for {key}.")); }
    }
    Ok(())
}

fn release_value(key: &str) -> Result<String, String> {
    let output = wsl_output(&["--distribution", DISTRO, "--user", USER, "--", "cat", RELEASE_PATH])?;
    if !output.status.success() { return Err("The Wallis distro is missing its rootfs release file. Reset Wallis and import a current archive.".into()); }
    let text = decode_wsl_output(&output.stdout);
    let prefix = format!("{key}=");
    let value = text.lines().find_map(|line| line.trim().strip_prefix(&prefix)).map(str::trim).unwrap_or_default();
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte)) { return Err(format!("The Wallis rootfs release file has no usable {key}.")); }
    Ok(value.to_string())
}

/// Runs a provisioning script inside the distro. The script arrives on stdin, which bash reads
/// as its own program — the one stdin shape WSL interop does hand through intact.
fn run_script(what: &str, script: &str) -> Result<String, String> {
    #[cfg(target_os = "windows")]
    {
        use std::io::Write;
        let mut child = command("wsl.exe").args(["--distribution", DISTRO, "--user", USER, "--", "bash", "-s"])
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
            .spawn().map_err(|error| format!("Unable to {what}: {error}"))?;
        child.stdin.take().ok_or("Unable to open the setup input.")?.write_all(script.as_bytes()).map_err(|error| format!("Unable to {what}: {error}"))?;
        let output = child.wait_with_output().map_err(|error| format!("Unable to {what}: {error}"))?;
        if !output.status.success() {
            let reason = decode_wsl_output(&output.stderr);
            return Err(format!("Wallis could not {what}: {}", reason.lines().filter(|line| !line.trim().is_empty()).next_back().unwrap_or("no output")));
        }
        Ok(decode_wsl_output(&output.stdout))
    }
    #[cfg(not(target_os = "windows"))]
    { let _ = (what, script); Err("Wallis currently requires Windows with WSL2.".into()) }
}

/// Downloads a pinned artifact, checks it, and unpacks it — skipped entirely once it is there.
fn fetch_script(marker: &str, url: &str, sha256: &str, unpack: &str) -> String {
    format!(
        "set -e\nW={WALLIS_HOME}\n[ -e \"{marker}\" ] && exit 0\nmkdir -p \"$W\" /tmp/wallis-setup\ncd /tmp/wallis-setup\ncurl -fsSL '{url}' -o artifact\necho '{sha256}  artifact' | sha256sum -c - >/dev/null\n{unpack}\nrm -f artifact\n"
    )
}

fn voice_steps(config: &WallisConfig) -> Vec<(&'static str, String)> {
    let (stt, stt_sha) = STT_MODELS.iter().find(|(id, ..)| *id == config.stt_model).map(|(id, _, sha, _)| (*id, *sha)).unwrap_or(("base.en", STT_MODELS[1].2));
    let (tts, tts_sha) = TTS_VOICES.iter().find(|(id, ..)| *id == config.tts_voice).map(|(id, _, sha, _)| (*id, *sha)).unwrap_or(("medium", TTS_VOICES[0].2));
    vec![
        ("Installing the bot runtime…", fetch_script(NODE_BIN, NODE_URL, NODE_SHA256, "rm -rf \"$W/node\"; mkdir -p \"$W/node\"; tar -xJf artifact -C \"$W/node\" --strip-components=1")),
        ("Installing speech recognition…", fetch_script(&format!("{WALLIS_HOME}/whisper/whisper-server"), WHISPER_URL, WHISPER_SHA256, "rm -rf \"$W/whisper\"; mkdir -p \"$W/whisper\"; tar -xzf artifact -C \"$W/whisper\" --strip-components=1")),
        ("Installing Wallis' voice…", format!("set -e
W={WALLIS_HOME}
[ -x \"$W/piper/bin/flask\" ] && exit 0
[ -x \"$W/piper/bin/piper\" ] || python3 -m venv \"$W/piper\"
\"$W/piper/bin/pip\" install --quiet --no-cache-dir 'piper-tts=={PIPER_VERSION}' 'flask=={FLASK_VERSION}'
")),
        ("Downloading the local voice models…", format!(
            "set -e
W={WALLIS_HOME}
mkdir -p \"$W/models\"
get() {{
  [ -f \"$3\" ] && return 0
  curl -fsSL \"$1\" -o \"$3.part\"
  [ -z \"$2\" ] || echo \"$2  $3.part\" | sha256sum -c - >/dev/null
  mv \"$3.part\" \"$3\"
}}
get '{STT_MODEL_BASE}ggml-{stt}.bin' '{stt_sha}' \"$W/models/ggml-{stt}.bin\"
get '{TTS_VOICE_BASE}{tts}/en_US-ryan-{tts}.onnx' '{tts_sha}' \"$W/models/en_US-ryan-{tts}.onnx\"
get '{TTS_VOICE_BASE}{tts}/en_US-ryan-{tts}.onnx.json' '' \"$W/models/en_US-ryan-{tts}.onnx.json\"
"
        )),
    ]
}

/// Installs everything the Discord bot needs inside the distro. Idempotent, and not user-facing
/// beyond the progress line: the first run downloads roughly 400 MB and takes a few minutes.
fn ensure_voice(state: &AppState) -> Result<(), String> {
    let config = state.config.lock().expect("config state poisoned").clone();
    for (label, script) in voice_steps(&config) {
        set_progress(state, label);
        run_script("install the voice runtime", &script)?;
    }
    set_progress(state, "Installing the Discord bot…");
    write_in_distro(BOT_PACKAGE, BOT_MANIFEST)?;
    write_in_distro(BOT_JS, BOT_SOURCE)?;
    run_script("install the Discord bot", &format!(
        "set -e\nW={WALLIS_HOME}\ncd \"$W/bot\"\nexport PATH=\"$W/node/bin:$PATH\"\n[ -d node_modules/discord.js ] || npm install --no-audit --no-fund --silent\n"
    ))?;
    set_progress(state, "");
    Ok(())
}

/// Installs and configures Mnemosyne only when it is not already live. Not user-facing.
fn ensure_memory() -> Result<(), String> {
    if memory_ready()? { return Ok(()); }
    let version = release_value("MNEMOSYNE_HERMES_VERSION")?;
    let output = wsl_output(&["--distribution", DISTRO, "--user", USER, "--", "/home/wallis/.hermes/bin/uv", "pip", "install", "--python", "/home/wallis/.hermes/hermes-agent/venv/bin/python", "--no-cache-dir", &format!("mnemosyne-hermes=={version}")])?;
    if !output.status.success() { return Err("Wallis could not install the Mnemosyne memory system in the distro.".into()); }
    hermes_config_set(&[("memory.provider", "mnemosyne"), ("memory.memory_enabled", "false"), ("user_profile_enabled", "false")])?;
    if !memory_ready()? { return Err("Mnemosyne was installed but is still not reporting ready.".into()); }
    Ok(())
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

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct VoiceOption { id: String, label: String, megabytes: u32 }

#[tauri::command]
fn get_voice_options() -> (Vec<VoiceOption>, Vec<VoiceOption>) {
    let map = |list: &[(&str, &str, &str, u32)]| list.iter().map(|(id, label, _, megabytes)| VoiceOption { id: (*id).to_string(), label: (*label).to_string(), megabytes: *megabytes }).collect();
    (map(&STT_MODELS), map(&TTS_VOICES))
}

fn provider_key_var(id: &str) -> Option<&'static str> { GUI_PROVIDER_KEYS.iter().find(|(provider, _)| *provider == id).map(|(_, key)| *key) }

fn validate_provider(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() || value.len() > 80 || !value.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte)) { return Err("Choose a provider reported by Hermes.".into()); }
    Ok(value.to_string())
}

fn validate_model(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() || value.len() > 120 || !value.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"._:-/".contains(&byte)) { return Err("Pick a model, or type a model id such as anthropic/claude-opus-5.".into()); }
    Ok(value.to_string())
}

fn validate_effort(value: &str) -> Result<String, String> {
    if EFFORTS.contains(&value) { Ok(value.to_string()) } else { Err("Pick a reasoning effort.".into()) }
}

/// Fetched with the distro's own curl so Wallis needs no HTTP client of its own.
fn fetch_json(url: &str) -> serde_json::Value {
    wsl_output(&["--distribution", DISTRO, "--user", USER, "--", "curl", "-fsSL", "--max-time", "20", url])
        .ok().filter(|output| output.status.success())
        .and_then(|output| serde_json::from_str(&decode_wsl_output(&output.stdout)).ok())
        .unwrap_or(serde_json::Value::Null)
}

/// OpenRouter model ids that publish no reasoning support. OpenRouter normalises every effort
/// level onto whatever a thinking model's own wire wants, so the only per-model question is
/// whether the model thinks at all. Only ids OpenRouter itself lists are judged: an id it does
/// not publish (a routing variant, or a failed lookup) keeps the full ladder rather than
/// being wrongly pinned to off.
fn silent_models() -> Vec<String> {
    fetch_json(OPENROUTER_MODELS_URL)["data"].as_array().map(|models| models.iter()
        .filter(|model| !model["supported_parameters"].as_array().is_some_and(|parameters| parameters.iter().any(|parameter| parameter.as_str() == Some("reasoning"))))
        .filter_map(|model| model["id"].as_str().map(str::to_string)).collect()).unwrap_or_default()
}

/// Ask the installed Hermes for the same authenticated providers and model catalogs its picker uses.
fn model_catalog(state: &AppState, refresh: bool) -> Result<Vec<ProviderOption>, String> {
    let current = state.config.lock().expect("config state poisoned").provider.clone();
    #[cfg(target_os = "windows")]
    let output = {
        let secret = provider_key(state).ok().flatten();
        let mut command = match secret.as_ref() {
            Some((key_var, key)) => wsl_with_secrets(&[(key_var, key)]),
            None => wsl_with_secrets(&[]),
        };
        command.args([HERMES_PYTHON, "-c", HERMES_CATALOG_SCRIPT, if refresh { "1" } else { "0" }, if current == "nous-api" { "nous" } else { &current }])
            .output().map_err(|error| format!("Unable to ask Hermes for providers: {error}"))?
    };
    #[cfg(not(target_os = "windows"))]
    { let _ = (state, refresh, current); return Err("Wallis currently requires Windows with WSL2.".into()); }
    if !output.status.success() { return Err(format!("Hermes could not refresh providers: {}", decode_wsl_output(&output.stderr).lines().last().unwrap_or("no output"))); }
    let mut providers: Vec<ProviderOption> = serde_json::from_str(&decode_wsl_output(&output.stdout)).map_err(|_| "Hermes returned an invalid provider catalog.".to_string())?;
    let silent = silent_models();
    if let Some(openrouter) = providers.iter_mut().find(|provider| provider.id == "openrouter") {
        for model in &mut openrouter.models {
            if silent.iter().any(|silent| silent == &model.id) { model.efforts = vec!["none".into()]; }
        }
    }
    Ok(providers)
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

fn generation_seed() -> String {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|elapsed| elapsed.as_nanos()).unwrap_or_default();
    format!("{:x}", nanos ^ (u128::from(std::process::id())).wrapping_mul(0x9E37_79B9_7F4A_7C15))
}

fn generation_prompt() -> String {
    format!(
        "Invent yourself. You are a fictional person whose first name is always {FIRST_NAME}; pick your own surname, age, home, work and personality. Randomness seed {seed} — let it push you somewhere unexpected, not the safest choice.\n\nReply with one JSON object and nothing else, using exactly these keys:\n{{\"name\": \"{FIRST_NAME} <surname>\", \"age\": \"27\", \"pronouns\": \"he/him\", \"location\": \"city, country\", \"occupation\": \"what you do\", \"tagline\": \"one line, under 90 characters\", \"bio\": \"2-4 sentences in your own voice\", \"interests\": [\"5 to 8 short specifics\"], \"lookingFor\": \"what you want from the people you talk to\", \"funFact\": \"one surprising detail\"}}\n\nWrite it like a dating profile: specific, warm, a little strange. No markdown, no code fences, no commentary.",
        seed = generation_seed(),
    )
}

/// WSL interop hands the distro a stdin that shell builtins cannot read, so secrets travel
/// as environment variables declared in WSLENV rather than on stdin or the command line.
#[cfg(target_os = "windows")]
fn wsl_with_secrets(secrets: &[(&str, &str)]) -> Command {
    let mut command = command("wsl.exe");
    for (name, value) in secrets { command.env(name, value); }
    command.env("WSLENV", secrets.iter().map(|(name, _)| *name).collect::<Vec<_>>().join(":"));
    command.args(["--distribution", DISTRO, "--user", USER, "--"]);
    command
}

/// Runs one Hermes turn, injecting a GUI-managed key when this provider uses one.
fn hermes_oneshot(provider_key: Option<(&str, &str)>, prompt: &str) -> Result<String, String> {
    #[cfg(target_os = "windows")]
    {
        let mut command = match provider_key { Some((key_var, key)) => wsl_with_secrets(&[(key_var, key)]), None => wsl_with_secrets(&[]) };
        let output = command.args([HERMES_BIN, "-z", prompt]).stdin(Stdio::null())
            .output().map_err(|error| format!("Unable to run Hermes: {error}"))?;
        if !output.status.success() { return Err(format!("Hermes could not answer ({}). Check the provider, model and API key.", decode_wsl_output(&output.stderr).lines().last().unwrap_or("no output"))); }
        Ok(decode_wsl_output(&output.stdout))
    }
    #[cfg(not(target_os = "windows"))]
    { let _ = (provider_key, prompt); Err("Wallis currently requires Windows with WSL2.".into()) }
}

fn read_profile() -> Result<Profile, String> {
    let output = wsl_output(&["--distribution", DISTRO, "--user", USER, "--", "cat", PROFILE_PATH])?;
    if !output.status.success() { return Err("Wallis has no profile yet.".into()); }
    serde_json::from_str(&decode_wsl_output(&output.stdout)).map(sanitize_profile).map_err(|_| "Wallis' profile file is not valid JSON.".into())
}

fn provider_key(state: &AppState) -> Result<Option<(&'static str, String)>, String> {
    let config = state.config.lock().expect("config state poisoned").clone();
    let Some(key_var) = provider_key_var(&config.provider) else { return Ok(None); };
    let app_data = state.app_data.lock().expect("app data state poisoned").clone().ok_or("Wallis configuration storage is not ready.")?;
    let key = unprotect_token(&key_path(&app_data))?;
    validate_secret(&key, "provider API key")?;
    Ok(Some((key_var, key)))
}

fn prerequisites(state: &AppState) -> Result<(String, String), String> {
    let config = state.config.lock().expect("config state poisoned").clone();
    let allowed = validate_allowed_users(&config.discord_allowed_users)?;
    let app_data = state.app_data.lock().expect("app data state poisoned").clone().ok_or("Wallis configuration storage is not ready.")?;
    if !token_path(&app_data).is_file() { return Err("No Discord token is configured. Save the bot token before starting Wallis.".into()); }
    let token = unprotect_token(&token_path(&app_data))?;
    validate_secret(&token, "Discord token")?;
    Ok((token, allowed))
}

fn check_wsl2() -> Result<(String, String), String> {
    let status = wsl_output(&["--status"])?;
    if !status.status.success() { return Err("WSL2 is unavailable. Install WSL2 from Windows Settings or Microsoft’s WSL instructions, then reboot if requested. Wallis will not enable Windows features automatically.".into()); }
    let version_output = wsl_output(&["--version"]).ok().filter(|output| output.status.success());
    let version_text = version_output.as_ref().map(|output| decode_wsl_output(&output.stdout)).unwrap_or_default();
    let status_text = decode_wsl_output(&status.stdout).to_ascii_lowercase();
    if version_output.is_none() && !status_text.contains("default version: 2") && !status_text.contains("kernel version") { return Err("WSL is installed, but WSL2 could not be confirmed. Update WSL from Windows, enable hardware virtualization, then restart Wallis. Wallis will not enable Windows features automatically.".into()); }
    Ok((parse_wsl_version(&version_text).unwrap_or_else(|| "unknown".into()), "WSL2 is available.".into()))
}

fn distro_info() -> Result<Option<String>, String> {
    let output = wsl_output(&["--list", "--verbose"])?;
    if !output.status.success() { return Err("WSL could not list installed distributions. Confirm WSL2 is installed and restart Wallis.".into()); }
    Ok(parse_distros(&decode_wsl_output(&output.stdout)).into_iter().find(|(name, _)| name.eq_ignore_ascii_case(DISTRO)).map(|(_, version)| version))
}

fn inspect_bot() -> Result<bool, String> {
    let output = wsl_output(&["--distribution", DISTRO, "--user", USER, "--", "ps", "-eo", "pid=,args="])?;
    if !output.status.success() { return Err("WSL could not inspect the Wallis bot. Confirm the non-root wallis user and ps are available.".into()); }
    Ok(bot_running(&decode_wsl_output(&output.stdout)))
}

fn bot_status() -> Option<serde_json::Value> {
    let output = wsl_output(&["--distribution", DISTRO, "--user", USER, "--", "cat", BOT_STATUS]).ok()?;
    if !output.status.success() { return None; }
    serde_json::from_str(&decode_wsl_output(&output.stdout)).ok()
}

fn bot_diagnostic() -> Option<String> {
    let value = bot_status()?;
    let error = value.get("lastError").filter(|error| !error.is_null())?;
    Some(format!("Last voice error ({}): {}", error.get("where")?.as_str()?, error.get("message")?.as_str()?))
}

/// Round trips the bot recorded, oldest first; the home page charts them per pipeline stage.
#[tauri::command]
fn get_voice_cycles() -> Vec<serde_json::Value> {
    bot_status().and_then(|value| Some(value.get("cycles")?.as_array()?.clone())).unwrap_or_default()
}

fn status(state: &str, message: impl Into<String>, discord_ready: bool, memory_ready: bool) -> RuntimeStatus { RuntimeStatus { state: state.into(), message: message.into(), discord_ready, memory_ready } }
fn stopped(message: impl Into<String>, discord_ready: bool, memory_ready: bool) -> RuntimeStatus { status("stopped", message, discord_ready, memory_ready) }

#[tauri::command]
fn get_setup_status() -> SetupStatus {
    match check_wsl2() { Ok((version, message)) => SetupStatus { wsl2_available: true, wsl_version: version, message }, Err(message) => SetupStatus { wsl2_available: false, wsl_version: "unknown".into(), message } }
}

#[tauri::command]
fn get_runtime_status(state: State<'_, AppState>) -> RuntimeStatus {
    if let Err(message) = check_wsl2() { return status("unavailable", message, false, false); }
    match distro_info() {
        Ok(None) => stopped("Wallis is not imported. Import a verified rootfs archive to continue.", false, false),
        Ok(Some(version)) => match require_wsl2_version(&version).and_then(|_| inspect_bot()) {
            Ok(running) => {
                let discord_ready = prerequisites(&state).is_ok();
                let memory = memory_ready().unwrap_or_else(|_| *state.memory_ready.lock().expect("memory state poisoned"));
                *state.memory_ready.lock().expect("memory state poisoned") = memory;
                if running {
                    let detail = bot_diagnostic().map(|detail| format!(" {detail}")).unwrap_or_default();
                    status("running", format!("Wallis is online as the non-root wallis user.{detail}"), discord_ready, memory)
                }
                else { stopped("Wallis is imported and set up; he is not running.", discord_ready, memory) }
            }
            Err(message) => status("unavailable", message, false, false),
        },
        Err(message) => status("unavailable", message, false, false),
    }
}

#[tauri::command(rename_all = "camelCase")]
fn import_wallis(rootfs_archive: String, expected_sha256: String, install_root: Option<String>) -> Result<RuntimeStatus, String> {
    let (archive, expected_sha256, root) = validate_import_inputs(&rootfs_archive, &expected_sha256, install_root.as_deref())?;
    let (_, _) = check_wsl2()?;
    if let Some(version) = distro_info()? { require_wsl2_version(&version)?; return Err("The Wallis distro is already imported. Use Reset Wallis first to remove it.".into()); }
    #[cfg(target_os = "windows")]
    {
        let path = archive.to_str().ok_or("The rootfs path is not valid Unicode.")?;
        let digest = command("certutil.exe").args(["-hashfile", path, "SHA256"]).output().map_err(|error| format!("Unable to verify the rootfs checksum: {error}"))?;
        if !digest.status.success() { return Err("Windows could not calculate the rootfs SHA-256 checksum.".into()); }
        let actual = extract_sha256(&decode_wsl_output(&digest.stdout)).ok_or("Windows returned no SHA-256 checksum.")?;
        if actual != expected_sha256 { return Err(format!("Rootfs checksum mismatch. Expected {expected_sha256}, got {actual}.")); }
        std::fs::create_dir_all(&root).map_err(|error| format!("Unable to create the distro install root: {error}"))?;
        let root = root.to_str().ok_or("The distro install root is not valid Unicode.")?;
        let imported = command("wsl.exe").args(["--import", DISTRO, root, path, "--version", "2"]).output().map_err(|error| format!("Unable to import the Wallis distro: {error}"))?;
        if !imported.status.success() { return Err("WSL rejected the Wallis rootfs import. Verify the archive is a Linux rootfs tar and WSL2 is supported.".into()); }
        apply_wsl_conf()?;
        Ok(stopped("Wallis imported as WSL2 and configured for the non-root wallis user.", false, false))
    }
    #[cfg(not(target_os = "windows"))]
    { let _ = (archive, expected_sha256, root); Err("Wallis currently requires Windows with WSL2.".into()) }
}

#[cfg(target_os = "windows")]
fn apply_wsl_conf() -> Result<(), String> {
    let mut child = command("wsl.exe").args(["--distribution", DISTRO, "--user", "root", "--", "tee", "/etc/wsl.conf"]).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped()).spawn().map_err(|error| format!("Unable to configure /etc/wsl.conf: {error}"))?;
    use std::io::Write;
    child.stdin.take().ok_or("Unable to open WSL configuration input.")?.write_all(WSL_CONF.as_bytes()).map_err(|error| format!("Unable to write /etc/wsl.conf: {error}"))?;
    let result = child.wait_with_output().map_err(|error| format!("Unable to apply /etc/wsl.conf: {error}"))?;
    if !result.status.success() { return Err("WSL could not write /etc/wsl.conf as root. The distro was imported; use Reset Wallis to retry cleanly.".into()); }
    let terminated = wsl_output(&["--terminate", DISTRO])?;
    if !terminated.status.success() { return Err("Wallis was imported but could not be restarted to apply /etc/wsl.conf.".into()); }
    Ok(())
}

/// Starts Wallis' own Discord bot inside the distro. Hermes stays headless: the bot calls it
/// per utterance, so only one process ever holds the Discord token.
fn start_wallis_impl(state: &AppState) -> Result<RuntimeStatus, String> {
    let (_, _) = check_wsl2()?;
    match distro_info()? { None => return Err("Wallis is not imported. Import a verified rootfs archive first.".into()), Some(version) => require_wsl2_version(&version)? }
    if !state.config.lock().expect("config state poisoned").onboarded { return Err("Finish setting Wallis up before starting him.".into()); }
    let application_id = state.config.lock().expect("config state poisoned").discord_application_id.clone();
    let (token, allowed) = prerequisites(state)?;
    let provider_key = provider_key(state)?;
    apply_soul(&read_profile()?)?;
    ensure_memory()?;
    ensure_voice(state)?;
    *state.memory_ready.lock().expect("memory state poisoned") = true;
    if inspect_bot()? { return Ok(status("running", "Wallis is already running as the non-root wallis user.", true, true)); }
    #[cfg(target_os = "windows")]
    {
        let voice = state.config.lock().expect("config state poisoned").clone();
        let (threads, speed, silence) = (voice.stt_threads.to_string(), voice.tts_speed.to_string(), voice.silence_ms.to_string());
        let mut secrets = vec![("DISCORD_BOT_TOKEN", token.as_str()), ("DISCORD_ALLOWED_USERS", allowed.as_str()), ("DISCORD_APPLICATION_ID", application_id.as_str()),
            ("WALLIS_STT_MODEL", voice.stt_model.as_str()), ("WALLIS_STT_THREADS", threads.as_str()), ("WALLIS_TTS_VOICE", voice.tts_voice.as_str()), ("WALLIS_TTS_SPEED", speed.as_str()), ("WALLIS_SILENCE_MS", silence.as_str())];
        if let Some((key_var, key)) = provider_key.as_ref() { secrets.push((key_var, key.as_str())); }
        let mut child = wsl_with_secrets(&secrets)
            .args([NODE_BIN, BOT_JS]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
            .spawn().map_err(|error| format!("Unable to start the Wallis bot: {error}"))?;
        for _ in 0..30 {
            if let Some(status) = child.try_wait().map_err(|error| format!("Unable to inspect the Wallis bot startup: {error}"))? {
                return Err(format!("The Wallis bot exited during startup ({status}). Check the Discord token and allow-list."));
            }
            let running = match inspect_bot() {
                Ok(running) => running,
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(error);
                }
            };
            if running {
                *state.bot.lock().expect("bot state poisoned") = Some(child);
                return Ok(status("running", "Wallis is online. Use /join in Discord to bring him into a voice channel.", true, true));
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        let _ = child.kill();
        let _ = child.wait();
        Err("The Wallis bot did not come up within 6 seconds. Check the Discord token and allow-list.".into())
    }
    #[cfg(not(target_os = "windows"))]
    { let _ = (state, application_id, token, allowed, provider_key); Err("Wallis currently requires Windows with WSL2.".into()) }
}

#[tauri::command]
fn stop_wallis(state: State<'_, AppState>) -> Result<RuntimeStatus, String> {
    #[cfg(target_os = "windows")]
    {
        let output = wsl_output(&["--terminate", DISTRO])?;
        if !output.status.success() { return Err("WSL could not stop the Wallis distribution.".into()); }
        *state.bot.lock().expect("bot state poisoned") = None;
        return Ok(stopped("Wallis stopped with his distribution.", prerequisites(&state).is_ok(), *state.memory_ready.lock().expect("memory state poisoned")));
    }
    #[cfg(not(target_os = "windows"))]
    { let _ = state; Err("Wallis currently requires Windows with WSL2.".into()) }
}

#[tauri::command]
fn reset_wallis(state: State<'_, AppState>) -> Result<RuntimeStatus, String> {
    #[cfg(target_os = "windows")]
    {
        let output = wsl_output(&["--unregister", DISTRO])?;
        if !output.status.success() { return Err("WSL could not unregister Wallis. If it is not imported, no reset is needed.".into()); }
        *state.bot.lock().expect("bot state poisoned") = None;
        *state.memory_ready.lock().expect("memory state poisoned") = false;
        let app_data = state.app_data.lock().expect("app data state poisoned").clone().ok_or("Wallis configuration storage is not ready.")?;
        let mut config = state.config.lock().expect("config state poisoned").clone();
        config.onboarded = false;
        save_config_file(&config_path(&app_data), &config)?;
        *state.config.lock().expect("config state poisoned") = config;
        return Ok(stopped("Wallis was unregistered. Its Linux data and memory were permanently deleted.", prerequisites(&state).is_ok(), false));
    }
    #[cfg(not(target_os = "windows"))]
    { let _ = state; Err("Wallis currently requires Windows with WSL2.".into()) }
}

#[tauri::command]
fn run_in_distro(command: String) -> Result<String, String> {
    if command.trim().is_empty() { return Err("Type a command to run.".into()); }
    let output = wsl_output(&["--distribution", DISTRO, "--user", USER, "--", "bash", "-lc", &command])?;
    let text = format!("{}{}", decode_wsl_output(&output.stdout), decode_wsl_output(&output.stderr));
    if text.trim().is_empty() { Ok(format!("(exited {})", output.status)) } else { Ok(text) }
}

#[tauri::command]
fn open_terminal() -> Result<(), String> {
    #[cfg(target_os = "windows")]
    { return Command::new("cmd").args(["/c", "start", "", "wsl.exe", "--distribution", DISTRO, "--user", USER]).spawn().map(|_| ()).map_err(|error| format!("Unable to open a terminal window: {error}")); }
    #[cfg(not(target_os = "windows"))]
    { Err("Wallis currently requires Windows with WSL2.".into()) }
}

#[tauri::command]
fn get_config(state: State<'_, AppState>) -> WallisConfig { state.config.lock().expect("config state poisoned").clone() }

#[tauri::command]
fn get_progress(state: State<'_, AppState>) -> String { state.progress.lock().expect("progress state poisoned").clone() }

#[tauri::command(rename_all = "camelCase")]
fn get_providers(refresh: Option<bool>, state: State<'_, AppState>) -> Result<Vec<ProviderOption>, String> { model_catalog(&state, refresh.unwrap_or(false)) }

/// Which screen the app owes the user: import the distro, choose inference, generate Wallis, or run.
#[tauri::command]
fn get_stage(state: State<'_, AppState>) -> String {
    if !matches!(distro_info(), Ok(Some(_))) { return "setup".into(); }
    let config = state.config.lock().expect("config state poisoned").clone();
    if provider_key(&state).is_err() || config.model.is_empty() { return "inference".into(); }
    if !config.onboarded || read_profile().is_err() { return "generate".into(); }
    "ready".into()
}

#[tauri::command]
fn get_profile() -> Result<Profile, String> { read_profile() }

#[tauri::command(rename_all = "camelCase")]
fn save_inference(provider: String, model: String, reasoning_effort: String, api_key: String, state: State<'_, AppState>) -> Result<WallisConfig, String> {
    let provider = validate_provider(&provider)?;
    let model = validate_model(&model)?;
    let reasoning_effort = validate_effort(reasoning_effort.trim())?;
    let app_data = state.app_data.lock().expect("app data state poisoned").clone().ok_or("Wallis configuration storage is not ready.")?;
    let api_key = api_key.trim();
    if !api_key.is_empty() {
        if provider_key_var(&provider).is_none() { return Err("Add this provider's credentials through the Hermes CLI, then refresh providers here.".into()); }
        validate_secret(api_key, "provider API key")?;
        protect_token(&key_path(&app_data), api_key)?;
    } else if provider_key_var(&provider).is_some() && !key_path(&app_data).is_file() {
        return Err("Add the API key for this provider.".into());
    }
    hermes_config_set(&[("model.provider", &provider), ("model.default", &model), ("agent.reasoning_effort", &reasoning_effort)])?;
    let mut config = state.config.lock().expect("config state poisoned").clone();
    (config.provider, config.model, config.reasoning_effort) = (provider, model, reasoning_effort);
    save_config_file(&config_path(&app_data), &config)?;
    *state.config.lock().expect("config state poisoned") = config.clone();
    Ok(config)
}

/// Long-running: installs memory if needed, has Hermes invent itself, then stores that identity.
#[tauri::command]
async fn generate_wallis(state: State<'_, AppState>) -> Result<Profile, String> {
    let provider_key = provider_key(&state)?;
    set_progress(&state, "Setting up memory…");
    ensure_memory()?;
    ensure_voice(&state)?;
    set_progress(&state, "Generating Wallis…");
    let answer = hermes_oneshot(provider_key.as_ref().map(|(key, value)| (*key, value.as_str())), &generation_prompt())?;
    let json = extract_json(&answer).ok_or_else(|| {
        let said = answer.split_whitespace().collect::<Vec<_>>().join(" ");
        if said.is_empty() { "Hermes answered with nothing at all. Check the model and API key, then try again.".to_string() }
        else { format!("Hermes did not return a profile. It said: {}", said.chars().take(200).collect::<String>()) }
    })?;
    let profile = sanitize_profile(serde_json::from_str(json).map_err(|error| format!("Hermes returned a profile Wallis could not read: {error}"))?);
    let stored = serde_json::to_string_pretty(&profile).map_err(|error| format!("Unable to serialize the profile: {error}"))?;
    write_in_distro(PROFILE_PATH, &stored)?;
    apply_soul(&profile)?;
    hermes_oneshot(provider_key.as_ref().map(|(key, value)| (*key, value.as_str())), &format!("This is who you are. Commit every detail to your long-term memory as durable facts about yourself, then reply with the word done.\n\n{stored}"))?;
    let app_data = state.app_data.lock().expect("app data state poisoned").clone().ok_or("Wallis configuration storage is not ready.")?;
    let mut config = state.config.lock().expect("config state poisoned").clone();
    config.onboarded = true;
    save_config_file(&config_path(&app_data), &config)?;
    *state.config.lock().expect("config state poisoned") = config;
    Ok(profile)
}

#[tauri::command]
fn save_config(config: WallisConfig, state: State<'_, AppState>) -> Result<WallisConfig, String> {
    if !config.rootfs_sha256.trim().is_empty() { validate_sha256(&config.rootfs_sha256)?; }
    let mut config = config;
    validate_voice(&mut config)?;
    config.discord_allowed_users = validate_allowed_users(&config.discord_allowed_users)?;
    let app_data = state.app_data.lock().expect("app data state poisoned").clone().ok_or("Wallis configuration storage is not ready.")?;
    save_config_file(&config_path(&app_data), &config)?;
    *state.config.lock().expect("config state poisoned") = config.clone();
    Ok(config)
}

#[tauri::command]
fn start_wallis(state: State<'_, AppState>) -> Result<RuntimeStatus, String> { start_wallis_impl(&state) }

#[tauri::command]
fn get_secret_status(state: State<'_, AppState>) -> Result<SecretStatus, String> {
    let app_data = state.app_data.lock().expect("app data state poisoned").clone().ok_or("Wallis configuration storage is not ready.")?;
    let path = token_path(&app_data);
    Ok(SecretStatus { has_token: path.is_file() && unprotect_token(&path).and_then(|token| { validate_secret(&token, "Discord token").map(|_| token) }).is_ok() })
}

#[tauri::command(rename_all = "camelCase")]
fn save_discord_token(token: String, state: State<'_, AppState>) -> Result<SecretStatus, String> {
    validate_secret(&token, "Discord token")?;
    let app_data = state.app_data.lock().expect("app data state poisoned").clone().ok_or("Wallis configuration storage is not ready.")?;
    std::fs::create_dir_all(&app_data).map_err(|error| format!("Unable to create the Wallis data folder: {error}"))?;
    protect_token(&token_path(&app_data), &token)?;
    Ok(SecretStatus { has_token: true })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_wsl_version_and_utf16_output() {
        assert_eq!(parse_wsl_version("WSL version: 2.5.9.0\nKernel version: 5.15"), Some("2.5.9.0".into()));
        let bytes: Vec<u8> = "WSL version: 2.5.9.0".encode_utf16().flat_map(u16::to_le_bytes).collect();
        assert_eq!(decode_wsl_output(&bytes), "WSL version: 2.5.9.0");
    }
    #[test]
    fn validates_and_extracts_sha256() {
        let checksum = "A".repeat(64);
        assert_eq!(validate_sha256(&checksum).unwrap(), checksum.to_ascii_lowercase());
        assert!(validate_sha256("not-a-checksum").is_err());
        assert_eq!(extract_sha256(&format!("CertUtil: -hashfile command\n{checksum}\nCertUtil: -hashfile completed")), Some(checksum.to_ascii_lowercase()));
    }
    #[test]
    fn parses_wsl_distro_versions() {
        assert_eq!(parse_distros("  NAME      STATE           VERSION\n* Wallis    Running         2\n  Ubuntu    Stopped         1"), vec![("Wallis".into(), "2".into()), ("Ubuntu".into(), "1".into())]);
        assert!(require_wsl2_version("1").is_err());
        assert!(require_wsl2_version("2").is_ok());
    }
    #[test]
    fn finds_only_the_wallis_bot_process() {
        assert!(bot_running("  12 /home/wallis/.wallis/node/bin/node /home/wallis/.wallis/bot/bot.mjs\n  13 ps -eo pid=,args="));
        assert!(!bot_running("  12 /home/wallis/.wallis/node/bin/node /home/wallis/.wallis/bot/other.mjs\n  13 ps"));
        assert!(!bot_running("  12 /usr/bin/python /home/wallis/.local/bin/hermes gateway"));
    }
    #[test]
    fn validates_allow_list_and_round_trips_non_secret_config() {
        assert_eq!(validate_allowed_users("123456789012").unwrap(), "123456789012");
        assert!(validate_allowed_users("all-users").is_err());
        let path = std::env::temp_dir().join(format!("wallis-config-test-{}.json", std::process::id()));
        let config = WallisConfig { discord_allowed_users: "123456789012".into(), ..WallisConfig::default() };
        save_config_file(&path, &config).unwrap();
        assert_eq!(load_config_file(&path).unwrap().discord_allowed_users, config.discord_allowed_users);
        let _ = std::fs::remove_file(path);
    }
    #[test]
    fn rejects_secret_values_that_could_escape_stdin() {
        assert!(validate_secret("token\nwith-newline", "Discord token").is_err());
        assert!(validate_secret("", "Discord token").is_err());
        assert!(validate_secret("token-value", "Discord token").is_ok());
    }
    #[test]
    fn provisioning_skips_what_is_installed_and_checks_what_it_downloads() {
        let steps = voice_steps(&WallisConfig::default());
        let node = &steps[0].1;
        assert!(node.contains(&format!("[ -e \"{NODE_BIN}\" ] && exit 0")), "node step must be idempotent");
        assert!(node.contains(NODE_SHA256) && node.contains("sha256sum -c"), "downloads must be verified");
        assert!(steps[3].1.contains(STT_MODELS[1].2) && steps[3].1.contains(TTS_VOICES[0].2), "the configured models must be the ones fetched");
        let mut small = WallisConfig { stt_model: "small.en".into(), tts_voice: "high".into(), ..WallisConfig::default() };
        assert!(voice_steps(&small)[3].1.contains(STT_MODELS[2].2));
        small.stt_threads = 0;
        assert!(validate_voice(&mut small).is_err());
        assert!(steps.iter().all(|(label, script)| !label.is_empty() && script.starts_with("set -e")));
    }
    #[test]
    fn locks_the_first_name_whatever_the_model_writes() {
        assert_eq!(lock_first_name("Wallis Vance"), "Wallis Vance");
        assert_eq!(lock_first_name("Margot Vance"), "Wallis Vance");
        assert_eq!(lock_first_name("Margot"), "Wallis");
        assert_eq!(lock_first_name(""), "Wallis");
        assert_eq!(lock_first_name("Margot\nVance de la Cruz"), "Wallis Vance de la Cruz");
    }
    #[test]
    fn extracts_one_json_object_from_a_chatty_answer() {
        assert_eq!(extract_json("Sure!\n{\"name\": \"Wallis\", \"nested\": {\"a\": 1}}\nHope that helps"), Some("{\"name\": \"Wallis\", \"nested\": {\"a\": 1}}"));
        assert_eq!(extract_json("{\"bio\": \"a } brace \\\" in a string\"}"), Some("{\"bio\": \"a } brace \\\" in a string\"}"));
        assert_eq!(extract_json("no json here"), None);
        assert_eq!(extract_json("{\"unterminated\": true"), None);
    }
    #[test]
    fn validates_inference_choices() {
        assert_eq!(validate_provider(" commandcode ").unwrap(), "commandcode");
        assert!(validate_provider("provider; rm -rf /").is_err());
        assert_eq!(validate_model(" anthropic/claude-opus-5 ").unwrap(), "anthropic/claude-opus-5");
        assert!(validate_model("model; rm -rf /").is_err());
        assert!(validate_effort("medium").is_ok());
        assert!(validate_effort("ludicrous").is_err());
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default().manage(AppState::default()).setup(|app| {
        let app_data = app.path().app_data_dir().map_err(std::io::Error::other)?;
        let config = load_config_file(&config_path(&app_data)).map_err(std::io::Error::other)?;
        let state = app.state::<AppState>();
        *state.config.lock().expect("config state poisoned") = config.clone();
        *state.app_data.lock().expect("app data state poisoned") = Some(app_data);
        // Off the main thread: starting the bot walks WSL, and the window should paint first.
        if config.auto_start {
            let handle = app.handle().clone();
            std::thread::spawn(move || { let _ = start_wallis_impl(&handle.state::<AppState>()); });
        }
        Ok(())
    }).invoke_handler(tauri::generate_handler![get_setup_status, get_runtime_status, import_wallis, start_wallis, stop_wallis, reset_wallis, get_config, save_config, get_secret_status, save_discord_token, run_in_distro, open_terminal, get_providers, get_profile, save_inference, generate_wallis, get_stage, get_progress, get_voice_options, get_voice_cycles]).run(tauri::generate_context!()).expect("error while running Wallis");
}
