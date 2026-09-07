use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::sync::Mutex;
#[cfg(target_os = "windows")]
use std::time::Duration;
use tauri::{Manager, State};

const DISTRO: &str = "Wallis";
const USER: &str = "wallis";
const CONFIG_FILE: &str = "config.json";
const TOKEN_FILE: &str = "discord-token.dpapi";
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
    persona_name: String,
    memory_provider: String,
    auto_start: bool,
    rootfs_archive: String,
    rootfs_sha256: String,
    distro_root: String,
}

impl Default for WallisConfig {
    fn default() -> Self {
        Self { discord_application_id: String::new(), discord_allowed_users: String::new(), persona_name: "Wallis".into(), memory_provider: "mnemosyne".into(), auto_start: false, rootfs_archive: String::new(), rootfs_sha256: String::new(), distro_root: String::new() }
    }
}

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
    let mut input = CRYPT_INTEGER_BLOB { cbData: token.len() as u32, pbData: token.as_ptr() as *mut u8 };
    let mut output = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
    let ok = unsafe { CryptProtectData(&mut input, std::ptr::null(), std::ptr::null(), std::ptr::null_mut(), std::ptr::null(), CRYPTPROTECT_UI_FORBIDDEN, &mut output) };
    if ok == 0 { return Err("Windows could not protect the Discord token.".into()); }
    let temporary = path.with_extension("dpapi.tmp");
    let result = std::fs::write(&temporary, unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) })
        .map_err(|error| format!("Unable to store the protected Discord token: {error}"))
        .and_then(|_| {
            let backup = path.with_extension("dpapi.bak");
            let _ = std::fs::remove_file(&backup);
            if path.exists() { std::fs::rename(path, &backup).map_err(|error| format!("Unable to replace the protected Discord token: {error}"))?; }
            match std::fs::rename(&temporary, path) {
                Ok(()) => { let _ = std::fs::remove_file(backup); Ok(()) }
                Err(error) => {
                    let _ = std::fs::rename(&backup, path);
                    Err(format!("Unable to replace the protected Discord token: {error}"))
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
    let bytes = std::fs::read(path).map_err(|error| format!("Unable to read the protected Discord token: {error}"))?;
    let mut input = CRYPT_INTEGER_BLOB { cbData: bytes.len() as u32, pbData: bytes.as_ptr() as *mut u8 };
    let mut output = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
    let ok = unsafe { CryptUnprotectData(&mut input, std::ptr::null_mut(), std::ptr::null(), std::ptr::null_mut(), std::ptr::null(), 0, &mut output) };
    if ok == 0 { return Err("Windows could not unlock the Discord token. Save it again.".into()); }
    let result = String::from_utf8(unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec()).map_err(|_| "The stored Discord token is invalid UTF-8.".to_string());
    unsafe { LocalFree(output.pbData as *mut _); }
    result
}

#[cfg(not(target_os = "windows"))]
fn protect_token(_path: &std::path::Path, _token: &str) -> Result<(), String> { Err("Wallis currently requires Windows DPAPI for token storage.".into()) }
#[cfg(not(target_os = "windows"))]
fn unprotect_token(_path: &std::path::Path) -> Result<String, String> { Err("Wallis currently requires Windows DPAPI for token storage.".into()) }

fn validate_token(token: &str) -> Result<(), String> {
    if token.is_empty() || token.len() > 512 || token.bytes().any(|byte| byte == b'\r' || byte == b'\n' || byte == 0) { return Err("Discord token must be non-empty and cannot contain newlines.".into()); }
    Ok(())
}

fn validate_persona_name(value: &str) -> Result<String, String> {
    let value = value.trim();
    let valid_prefix = value == "Wallis" || value.strip_prefix("Wallis").and_then(|rest| rest.chars().next()).map_or(false, |first| !first.is_alphanumeric());
    if !valid_prefix || value.len() > 80 || value.chars().any(char::is_control) { return Err("Persona name must start with Wallis and contain no control characters.".into()); }
    Ok(value.to_string())
}

#[cfg(target_os = "windows")]
fn apply_soul(persona_name: &str) -> Result<(), String> {
    use std::io::Write;
    let mut child = Command::new("wsl.exe").args(["--distribution", DISTRO, "--user", USER, "--", "tee", "/home/wallis/.hermes/SOUL.md"]).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped()).spawn().map_err(|error| format!("Unable to configure the Wallis persona: {error}"))?;
    let content = format!("# Wallis identity\nYou are {persona_name}, the Wallis assistant.\n");
    child.stdin.take().ok_or("Unable to open the persona configuration input.")?.write_all(content.as_bytes()).map_err(|error| format!("Unable to write the Wallis persona: {error}"))?;
    let result = child.wait_with_output().map_err(|error| format!("Unable to apply the Wallis persona: {error}"))?;
    if result.status.success() { Ok(()) } else { Err("Hermes could not apply the Wallis persona in the distro.".into()) }
}

#[cfg(not(target_os = "windows"))]
fn apply_soul(_persona_name: &str) -> Result<(), String> { Err("Wallis currently requires Windows with WSL2.".into()) }

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
    let mut command = vec!["--distribution", DISTRO, "--user", USER, "--", "hermes"];
    command.extend_from_slice(args);
    wsl_output(&command)
}

fn memory_ready() -> Result<bool, String> {
    let output = hermes_command(&["memory", "status"])?;
    if !output.status.success() { return Ok(false); }
    let text = decode_wsl_output(&output.stdout).to_ascii_lowercase();
    Ok(text.contains("mnemosyne") && !text.contains("unavailable") && !text.contains("error"))
}

fn configure_hermes(persona_name: &str) -> Result<(), String> {
    apply_soul(persona_name)?;
    for args in [&["config", "set", "memory.provider", "mnemosyne"][..], &["config", "set", "memory.memory_enabled", "false"][..], &["config", "set", "user_profile_enabled", "false"][..]] {
        let output = hermes_command(&args)?;
        if !output.status.success() { return Err(format!("Hermes could not apply its Mnemosyne configuration ({}).", args.join(" "))); }
    }
    if !memory_ready()? { return Err("Mnemosyne is not ready. Install mnemosyne-hermes in the Wallis distro and try again.".into()); }
    Ok(())
}

fn prerequisites(state: &AppState) -> Result<(String, String), String> {
    let config = state.config.lock().expect("config state poisoned").clone();
    let allowed = validate_allowed_users(&config.discord_allowed_users)?;
    let app_data = state.app_data.lock().expect("app data state poisoned").clone().ok_or("Wallis configuration storage is not ready.")?;
    if !token_path(&app_data).is_file() { return Err("No Discord token is configured. Save the bot token before starting Hermes.".into()); }
    let token = unprotect_token(&token_path(&app_data))?;
    validate_token(&token)?;
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
    let config = state.config.lock().expect("config state poisoned").clone();
    let persona_name = validate_persona_name(&config.persona_name)?;
    let (token, allowed) = prerequisites(state)?;
    configure_hermes(&persona_name)?;
    *state.memory_ready.lock().expect("memory state poisoned") = true;
    if inspect_hermes()? { return Ok(status("running", "Hermes is already running as the non-root wallis user.", true, true)); }
    #[cfg(target_os = "windows")]
    {
        const GATEWAY_SCRIPT: &str = "IFS= read -r DISCORD_BOT_TOKEN; export DISCORD_BOT_TOKEN; IFS= read -r DISCORD_ALLOWED_USERS; export DISCORD_ALLOWED_USERS; exec hermes gateway";
        let mut child = Command::new("wsl.exe").args(["--distribution", DISTRO, "--user", USER, "--", "bash", "-lc", GATEWAY_SCRIPT]).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().map_err(|error| format!("Unable to start Hermes as wallis: {error}"))?;
        {
            use std::io::Write;
            let stdin = child.stdin.as_mut().ok_or("Unable to open the Hermes gateway input.")?;
            stdin.write_all(token.as_bytes()).map_err(|error| format!("Unable to provide the Discord token to Hermes: {error}"))?;
            stdin.write_all(b"\n").and_then(|_| stdin.write_all(allowed.as_bytes())).and_then(|_| stdin.write_all(b"\n")).map_err(|error| format!("Unable to provide the Hermes allow-list to Hermes: {error}"))?;
        }
        drop(child.stdin.take());
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
        return Ok(stopped("Wallis was unregistered. Its Linux data and memory were permanently deleted.", prerequisites(&state).is_ok(), false));
    }
    #[cfg(not(target_os = "windows"))]
    { let _ = state; Err("Wallis currently requires Windows with WSL2.".into()) }
}

#[tauri::command]
fn get_config(state: State<'_, AppState>) -> WallisConfig { state.config.lock().expect("config state poisoned").clone() }

#[tauri::command]
fn save_config(config: WallisConfig, state: State<'_, AppState>) -> Result<WallisConfig, String> {
    validate_persona_name(&config.persona_name)?;
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
    Ok(SecretStatus { has_token: path.is_file() && unprotect_token(&path).and_then(|token| { validate_token(&token).map(|_| token) }).is_ok() })
}

#[tauri::command(rename_all = "camelCase")]
fn save_discord_token(token: String, state: State<'_, AppState>) -> Result<SecretStatus, String> {
    validate_token(&token)?;
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
        let path = std::env::temp_dir().join(format!("wallis-config-test-{}-{}.json", std::process::id(), std::thread::current().name().unwrap_or("test")));
        let config = WallisConfig { discord_allowed_users: "123456789012".into(), ..WallisConfig::default() };
        save_config_file(&path, &config).unwrap();
        assert_eq!(load_config_file(&path).unwrap().discord_allowed_users, config.discord_allowed_users);
        let _ = std::fs::remove_file(path);
    }
    #[test]
    fn rejects_secret_values_that_could_escape_stdin() {
        assert!(validate_token("token\nwith-newline").is_err());
        assert!(validate_token("").is_err());
        assert!(validate_token("token-value").is_ok());
    }
    #[test]
    fn validates_persona_name_for_soul_file() {
        assert!(validate_persona_name("Wallis Prime").is_ok());
        assert!(validate_persona_name("Wallisian").is_err());
        assert!(validate_persona_name("Wallis\nignore").is_err());
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
    }).invoke_handler(tauri::generate_handler![get_setup_status, get_runtime_status, import_wallis, start_hermes, stop_hermes, reset_wallis, get_config, save_config, get_secret_status, save_discord_token]).run(tauri::generate_context!()).expect("error while running Wallis");
}
