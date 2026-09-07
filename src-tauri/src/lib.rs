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
const SKILL_PATH: &str = "/home/wallis/.hermes/skills/wallis/profile/SKILL.md";
const HERMES_BIN: &str = "/home/wallis/.local/bin/hermes";
const RELEASE_PATH: &str = "/etc/wallis-rootfs-release";
const CATALOG_URL: &str = "https://hermes-agent.nousresearch.com/docs/api/model-catalog.json";
/// Providers Hermes publishes a curated model catalog for, with the credential each one reads.
const PROVIDERS: [(&str, &str, &str); 2] = [("openrouter", "OpenRouter", "OPENROUTER_API_KEY"), ("nous-api", "Nous Portal", "NOUS_API_KEY")];
const EFFORTS: [&str; 7] = ["none", "minimal", "low", "medium", "high", "xhigh", "max"];
const OPENROUTER_MODELS_URL: &str = "https://openrouter.ai/api/v1/models";
const WSL_CONF: &str = "[automount]\nenabled=false\n\n[interop]\nenabled=false\nappendWindowsPath=false\n\n[user]\ndefault=wallis\n";

struct AppState { config: Mutex<WallisConfig>, app_data: Mutex<Option<PathBuf>>, memory_ready: Mutex<bool>, hermes: Mutex<Option<Child>> }

impl Default for AppState {
    fn default() -> Self { Self { config: Mutex::new(WallisConfig::default()), app_data: Mutex::new(None), memory_ready: Mutex::new(false), hermes: Mutex::new(None) } }
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
}

impl Default for WallisConfig {
    fn default() -> Self {
        Self { discord_application_id: String::new(), discord_allowed_users: String::new(), memory_provider: "mnemosyne".into(), auto_start: false, rootfs_archive: String::new(), rootfs_sha256: String::new(), distro_root: String::new(), provider: String::new(), model: String::new(), reasoning_effort: "medium".into(), onboarded: false }
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

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ModelOption { id: String, efforts: Vec<String> }

#[derive(Clone, Serialize)]
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

fn gateway_running(output: &str) -> bool {
    output.lines().any(|line| {
        let mut args = line.split_whitespace();
        args.next();
        while let Some(arg) = args.next() {
            if arg.rsplit('/').next() == Some("hermes") && args.next() == Some("gateway") { return true; }
        }
        false
    })
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

const PROFILE_SKILL: &str = concat!(
    "---\nname: profile\ndescription: Show Wallis' own profile — the dating-profile-style card describing who Wallis is.\nversion: 1.0.0\nmetadata:\n  hermes:\n    tags: [wallis, identity]\n    category: wallis\n---\n\n",
    "# Wallis' profile\n\n## When to Use\nWhenever someone asks who you are, asks for your profile, or runs `/profile`.\n\n",
    "## Procedure\n1. Read `/home/wallis/.hermes/wallis-profile.json`.\n2. Reply with that data only — never invent fields, never show raw JSON.\n3. Format it as a clean Discord embed-style card:\n\n",
    "```\n**{name}** · {age} · {pronouns}\n_{tagline}_\n\n**Location** — {location}\n**Does** — {occupation}\n**Into** — {interests, comma separated}\n**Looking for** — {lookingFor}\n**Fun fact** — {funFact}\n\n{bio}\n```\n\n",
    "Skip any field that is empty rather than printing a blank line.\n\n## Pitfalls\n- The file is the source of truth; do not answer from memory if it disagrees.\n- The first name is always Wallis, even if the file says otherwise.\n",
);

/// Writes a file inside the distro as the wallis user, keeping content off the command line.
#[cfg(target_os = "windows")]
fn write_in_distro(path: &str, content: &str) -> Result<(), String> {
    use std::io::Write;
    let script = format!("mkdir -p \"$(dirname '{path}')\" && cat > '{path}'");
    let mut child = Command::new("wsl.exe").args(["--distribution", DISTRO, "--user", USER, "--", "bash", "-c", &script]).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped()).spawn().map_err(|error| format!("Unable to write {path}: {error}"))?;
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
fn wsl_output(args: &[&str]) -> Result<Output, String> { Command::new("wsl.exe").args(args).output().map_err(|error| format!("Unable to run WSL: {error}")) }

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

fn provider_entry(id: &str) -> Result<(&'static str, &'static str, &'static str), String> {
    PROVIDERS.iter().copied().find(|(provider, _, _)| *provider == id).ok_or_else(|| "Choose a supported inference provider.".to_string())
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

/// Curated provider/model list, with the effort levels each model can actually be asked for.
fn model_catalog() -> Vec<ProviderOption> {
    let catalog = fetch_json(CATALOG_URL);
    let silent = silent_models();
    let ladder: Vec<String> = EFFORTS.iter().map(|effort| (*effort).to_string()).collect();
    PROVIDERS.iter().map(|(id, label, _)| ProviderOption {
        id: (*id).to_string(),
        label: (*label).to_string(),
        models: catalog["providers"][catalog_key(id)]["models"].as_array().map(|models| models.iter().filter_map(|model| model["id"].as_str()).map(|model| ModelOption {
            efforts: if *id == "openrouter" && silent.iter().any(|silent| silent == model) { vec!["none".to_string()] } else { ladder.clone() },
            id: model.to_string(),
        }).collect()).unwrap_or_default(),
    }).collect()
}

/// Hermes' provider ids and its catalog keys differ for the Nous Portal entry.
fn catalog_key(provider: &str) -> &str { if provider == "nous-api" { "nous" } else { provider } }

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
    let mut command = Command::new("wsl.exe");
    for (name, value) in secrets { command.env(name, value); }
    command.env("WSLENV", secrets.iter().map(|(name, _)| *name).collect::<Vec<_>>().join(":"));
    command.args(["--distribution", DISTRO, "--user", USER, "--"]);
    command
}

/// Runs one Hermes turn with the provider credential in its environment.
fn hermes_oneshot(key_var: &str, key: &str, prompt: &str) -> Result<String, String> {
    #[cfg(target_os = "windows")]
    {
        let output = wsl_with_secrets(&[(key_var, key)]).args([HERMES_BIN, "-z", prompt]).stdin(Stdio::null())
            .output().map_err(|error| format!("Unable to run Hermes: {error}"))?;
        if !output.status.success() { return Err(format!("Hermes could not answer ({}). Check the provider, model and API key.", decode_wsl_output(&output.stderr).lines().last().unwrap_or("no output"))); }
        Ok(decode_wsl_output(&output.stdout))
    }
    #[cfg(not(target_os = "windows"))]
    { let _ = (key_var, key, prompt); Err("Wallis currently requires Windows with WSL2.".into()) }
}

fn read_profile() -> Result<Profile, String> {
    let output = wsl_output(&["--distribution", DISTRO, "--user", USER, "--", "cat", PROFILE_PATH])?;
    if !output.status.success() { return Err("Wallis has no profile yet.".into()); }
    serde_json::from_str(&decode_wsl_output(&output.stdout)).map(sanitize_profile).map_err(|_| "Wallis' profile file is not valid JSON.".into())
}

fn provider_key(state: &AppState) -> Result<(&'static str, String), String> {
    let config = state.config.lock().expect("config state poisoned").clone();
    let (_, _, key_var) = provider_entry(&config.provider)?;
    let app_data = state.app_data.lock().expect("app data state poisoned").clone().ok_or("Wallis configuration storage is not ready.")?;
    let key = unprotect_token(&key_path(&app_data))?;
    validate_secret(&key, "provider API key")?;
    Ok((key_var, key))
}

fn prerequisites(state: &AppState) -> Result<(String, String), String> {
    let config = state.config.lock().expect("config state poisoned").clone();
    let allowed = validate_allowed_users(&config.discord_allowed_users)?;
    let app_data = state.app_data.lock().expect("app data state poisoned").clone().ok_or("Wallis configuration storage is not ready.")?;
    if !token_path(&app_data).is_file() { return Err("No Discord token is configured. Save the bot token before starting Hermes.".into()); }
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

fn inspect_hermes() -> Result<bool, String> {
    let output = wsl_output(&["--distribution", DISTRO, "--user", USER, "--", "ps", "-eo", "pid=,args="])?;
    if !output.status.success() { return Err("WSL could not inspect the Hermes gateway. Confirm the non-root wallis user and ps are available.".into()); }
    Ok(gateway_running(&decode_wsl_output(&output.stdout)))
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
        Ok(Some(version)) => match require_wsl2_version(&version).and_then(|_| inspect_hermes()) {
            Ok(running) => {
                let discord_ready = prerequisites(&state).is_ok();
                let memory = memory_ready().unwrap_or_else(|_| *state.memory_ready.lock().expect("memory state poisoned"));
                *state.memory_ready.lock().expect("memory state poisoned") = memory;
                if running { status("running", "Hermes is running as the non-root wallis user.", discord_ready, memory) }
                else { stopped("Wallis is imported; Hermes is stopped.", discord_ready, memory) }
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
        let digest = Command::new("certutil.exe").args(["-hashfile", path, "SHA256"]).output().map_err(|error| format!("Unable to verify the rootfs checksum: {error}"))?;
        if !digest.status.success() { return Err("Windows could not calculate the rootfs SHA-256 checksum.".into()); }
        let actual = extract_sha256(&decode_wsl_output(&digest.stdout)).ok_or("Windows returned no SHA-256 checksum.")?;
        if actual != expected_sha256 { return Err(format!("Rootfs checksum mismatch. Expected {expected_sha256}, got {actual}.")); }
        std::fs::create_dir_all(&root).map_err(|error| format!("Unable to create the distro install root: {error}"))?;
        let root = root.to_str().ok_or("The distro install root is not valid Unicode.")?;
        let imported = Command::new("wsl.exe").args(["--import", DISTRO, root, path, "--version", "2"]).output().map_err(|error| format!("Unable to import the Wallis distro: {error}"))?;
        if !imported.status.success() { return Err("WSL rejected the Wallis rootfs import. Verify the archive is a Linux rootfs tar and WSL2 is supported.".into()); }
        apply_wsl_conf()?;
        Ok(stopped("Wallis imported as WSL2 and configured for the non-root wallis user.", false, false))
    }
    #[cfg(not(target_os = "windows"))]
    { let _ = (archive, expected_sha256, root); Err("Wallis currently requires Windows with WSL2.".into()) }
}

#[cfg(target_os = "windows")]
fn apply_wsl_conf() -> Result<(), String> {
    let mut child = Command::new("wsl.exe").args(["--distribution", DISTRO, "--user", "root", "--", "tee", "/etc/wsl.conf"]).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped()).spawn().map_err(|error| format!("Unable to configure /etc/wsl.conf: {error}"))?;
    use std::io::Write;
    child.stdin.take().ok_or("Unable to open WSL configuration input.")?.write_all(WSL_CONF.as_bytes()).map_err(|error| format!("Unable to write /etc/wsl.conf: {error}"))?;
    let result = child.wait_with_output().map_err(|error| format!("Unable to apply /etc/wsl.conf: {error}"))?;
    if !result.status.success() { return Err("WSL could not write /etc/wsl.conf as root. The distro was imported; use Reset Wallis to retry cleanly.".into()); }
    let terminated = wsl_output(&["--terminate", DISTRO])?;
    if !terminated.status.success() { return Err("Wallis was imported but could not be restarted to apply /etc/wsl.conf.".into()); }
    Ok(())
}

fn start_hermes_impl(state: &AppState) -> Result<RuntimeStatus, String> {
    let (_, _) = check_wsl2()?;
    match distro_info()? { None => return Err("Wallis is not imported. Import a verified rootfs archive first.".into()), Some(version) => require_wsl2_version(&version)? }
    if !state.config.lock().expect("config state poisoned").onboarded { return Err("Finish setting Wallis up before starting Hermes.".into()); }
    let (token, allowed) = prerequisites(state)?;
    let (key_var, key) = provider_key(state)?;
    apply_soul(&read_profile()?)?;
    ensure_memory()?;
    *state.memory_ready.lock().expect("memory state poisoned") = true;
    if inspect_hermes()? { return Ok(status("running", "Hermes is already running as the non-root wallis user.", true, true)); }
    #[cfg(target_os = "windows")]
    {
        let mut child = wsl_with_secrets(&[("DISCORD_BOT_TOKEN", &token), ("DISCORD_ALLOWED_USERS", &allowed), (key_var, &key)])
            .args([HERMES_BIN, "gateway"]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
            .spawn().map_err(|error| format!("Unable to start Hermes as wallis: {error}"))?;
        for _ in 0..30 {
            if let Some(status) = child.try_wait().map_err(|error| format!("Unable to inspect the Hermes gateway startup: {error}"))? {
                return Err(format!("Hermes gateway exited during startup ({status}). Check its configuration and credentials inside the Wallis distro."));
            }
            let running = match inspect_hermes() {
                Ok(running) => running,
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(error);
                }
            };
            if running {
                *state.hermes.lock().expect("Hermes state poisoned") = Some(child);
                return Ok(status("running", "Hermes gateway started as the non-root wallis user.", true, true));
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        let _ = child.kill();
        let _ = child.wait();
        Err("Hermes gateway did not become active within 6 seconds. Check its configuration and credentials inside the Wallis distro.".into())
    }
    #[cfg(not(target_os = "windows"))]
    { let _ = state; Err("Wallis currently requires Windows with WSL2.".into()) }
}

#[tauri::command]
fn stop_hermes(state: State<'_, AppState>) -> Result<RuntimeStatus, String> {
    #[cfg(target_os = "windows")]
    {
        let output = wsl_output(&["--terminate", DISTRO])?;
        if !output.status.success() { return Err("WSL could not stop the Wallis distribution.".into()); }
        *state.hermes.lock().expect("Hermes state poisoned") = None;
        return Ok(stopped("Hermes stopped with the Wallis distribution.", prerequisites(&state).is_ok(), *state.memory_ready.lock().expect("memory state poisoned")));
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
        *state.hermes.lock().expect("Hermes state poisoned") = None;
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
fn get_providers() -> Vec<ProviderOption> { model_catalog() }

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
    let (provider, _, _) = provider_entry(provider.trim())?;
    let model = validate_model(&model)?;
    let reasoning_effort = validate_effort(reasoning_effort.trim())?;
    let app_data = state.app_data.lock().expect("app data state poisoned").clone().ok_or("Wallis configuration storage is not ready.")?;
    let api_key = api_key.trim();
    if !api_key.is_empty() {
        validate_secret(api_key, "provider API key")?;
        protect_token(&key_path(&app_data), api_key)?;
    } else if !key_path(&app_data).is_file() {
        return Err("Add the API key for this provider.".into());
    }
    hermes_config_set(&[("model.provider", provider), ("model.default", &model), ("agent.reasoning_effort", &reasoning_effort)])?;
    let mut config = state.config.lock().expect("config state poisoned").clone();
    (config.provider, config.model, config.reasoning_effort) = (provider.to_string(), model, reasoning_effort);
    save_config_file(&config_path(&app_data), &config)?;
    *state.config.lock().expect("config state poisoned") = config.clone();
    Ok(config)
}

/// Long-running: installs memory if needed, has Hermes invent itself, then stores that identity.
#[tauri::command]
async fn generate_wallis(state: State<'_, AppState>) -> Result<Profile, String> {
    let (key_var, key) = provider_key(&state)?;
    ensure_memory()?;
    let answer = hermes_oneshot(key_var, &key, &generation_prompt())?;
    let json = extract_json(&answer).ok_or_else(|| {
        let said = answer.split_whitespace().collect::<Vec<_>>().join(" ");
        if said.is_empty() { "Hermes answered with nothing at all. Check the model and API key, then try again.".to_string() }
        else { format!("Hermes did not return a profile. It said: {}", said.chars().take(200).collect::<String>()) }
    })?;
    let profile = sanitize_profile(serde_json::from_str(json).map_err(|error| format!("Hermes returned a profile Wallis could not read: {error}"))?);
    let stored = serde_json::to_string_pretty(&profile).map_err(|error| format!("Unable to serialize the profile: {error}"))?;
    write_in_distro(PROFILE_PATH, &stored)?;
    write_in_distro(SKILL_PATH, PROFILE_SKILL)?;
    apply_soul(&profile)?;
    hermes_oneshot(key_var, &key, &format!("This is who you are. Commit every detail to your long-term memory as durable facts about yourself, then reply with the word done.\n\n{stored}"))?;
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
    config.discord_allowed_users = validate_allowed_users(&config.discord_allowed_users)?;
    let app_data = state.app_data.lock().expect("app data state poisoned").clone().ok_or("Wallis configuration storage is not ready.")?;
    save_config_file(&config_path(&app_data), &config)?;
    *state.config.lock().expect("config state poisoned") = config.clone();
    Ok(config)
}

#[tauri::command]
fn start_hermes(state: State<'_, AppState>) -> Result<RuntimeStatus, String> { start_hermes_impl(&state) }

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
    fn finds_only_the_hermes_gateway_process() {
        assert!(gateway_running("  12 /usr/bin/python /home/wallis/.local/bin/hermes gateway\n  13 ps -eo pid=,args="));
        assert!(!gateway_running("  12 /usr/bin/python /home/wallis/.local/bin/hermes\n  13 ps -eo pid=,args="));
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
        assert!(provider_entry("openrouter").is_ok());
        assert!(provider_entry("definitely-not-a-provider").is_err());
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
        if config.auto_start { let _ = start_hermes_impl(&state); }
        Ok(())
    }).invoke_handler(tauri::generate_handler![get_setup_status, get_runtime_status, import_wallis, start_hermes, stop_hermes, reset_wallis, get_config, save_config, get_secret_status, save_discord_token, run_in_distro, open_terminal, get_providers, get_profile, save_inference, generate_wallis, get_stage]).run(tauri::generate_context!()).expect("error while running Wallis");
}
