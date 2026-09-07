use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::sync::Mutex;
use tauri::State;

const DISTRO: &str = "Wallis";
const USER: &str = "wallis";
const WSL_CONF: &str = "[automount]\nenabled=false\n\n[interop]\nenabled=false\nappendWindowsPath=false\n\n[user]\ndefault=wallis\n";

#[derive(Default)]
struct AppState { config: Mutex<WallisConfig>, hermes: Mutex<Option<Child>> }

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct WallisConfig {
    discord_application_id: String,
    persona_name: String,
    memory_provider: String,
    auto_start: bool,
    rootfs_archive: String,
    rootfs_sha256: String,
    distro_root: String,
}

impl Default for WallisConfig {
    fn default() -> Self {
        Self { discord_application_id: String::new(), persona_name: "Wallis".into(), memory_provider: "mnemosyne".into(), auto_start: false, rootfs_archive: String::new(), rootfs_sha256: String::new(), distro_root: String::new() }
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SetupStatus { wsl2_available: bool, wsl_version: String, message: String }

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeStatus { state: String, message: String }

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
    let output = wsl_output(&["--distribution", DISTRO, "--user", USER, "--", "pgrep", "-x", "hermes"])?;
    if output.status.success() { Ok(true) } else if output.status.code() == Some(1) { Ok(false) } else { Err("WSL could not inspect Hermes. Confirm the Wallis distro contains the non-root wallis user and pgrep.".into()) }
}

fn stopped(message: impl Into<String>) -> RuntimeStatus { RuntimeStatus { state: "stopped".into(), message: message.into() } }

#[tauri::command]
fn get_setup_status() -> SetupStatus {
    match check_wsl2() { Ok((version, message)) => SetupStatus { wsl2_available: true, wsl_version: version, message }, Err(message) => SetupStatus { wsl2_available: false, wsl_version: "unknown".into(), message } }
}

#[tauri::command]
fn get_runtime_status() -> RuntimeStatus {
    if let Err(message) = check_wsl2() { return RuntimeStatus { state: "unavailable".into(), message }; }
    match distro_info() {
        Ok(None) => stopped("Wallis is not imported. Import a verified rootfs archive to continue."),
        Ok(Some(_)) => match inspect_hermes() {
            Ok(true) => RuntimeStatus { state: "running".into(), message: "Hermes is running as the non-root wallis user.".into() },
            Ok(false) => stopped("Wallis is imported; Hermes is stopped."),
            Err(message) => RuntimeStatus { state: "unavailable".into(), message },
        },
        Err(message) => RuntimeStatus { state: "unavailable".into(), message },
    }
}

#[tauri::command(rename_all = "camelCase")]
fn import_wallis(rootfs_archive: String, expected_sha256: String, install_root: Option<String>) -> Result<RuntimeStatus, String> {
    let (archive, expected_sha256, root) = validate_import_inputs(&rootfs_archive, &expected_sha256, install_root.as_deref())?;
    let (_, _) = check_wsl2()?;
    if distro_info()?.is_some() { return Err("The Wallis distro is already imported. Use Reset Wallis first to remove it.".into()); }
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
        Ok(stopped("Wallis imported as WSL2 and configured for the non-root wallis user."))
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

#[tauri::command]
fn start_hermes(state: State<'_, AppState>) -> Result<RuntimeStatus, String> {
    let (_, _) = check_wsl2()?;
    if distro_info()?.is_none() { return Err("Wallis is not imported. Import a verified rootfs archive first.".into()); }
    if inspect_hermes()? { return Ok(RuntimeStatus { state: "running".into(), message: "Hermes is already running as the non-root wallis user.".into() }); }
    #[cfg(target_os = "windows")]
    {
        let child = Command::new("wsl.exe").args(["--distribution", DISTRO, "--user", USER, "--", "hermes", "gateway"]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().map_err(|error| format!("Unable to start Hermes as wallis: {error}"))?;
        *state.hermes.lock().expect("Hermes state poisoned") = Some(child);
        Ok(RuntimeStatus { state: "running".into(), message: "Hermes started as the non-root wallis user.".into() })
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
        return Ok(stopped("Hermes stopped with the Wallis distribution."));
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
        return Ok(stopped("Wallis was unregistered. Its Linux data and memory were permanently deleted."));
    }
    #[cfg(not(target_os = "windows"))]
    { let _ = state; Err("Wallis currently requires Windows with WSL2.".into()) }
}

#[tauri::command]
fn get_config(state: State<'_, AppState>) -> WallisConfig { state.config.lock().expect("config state poisoned").clone() }

#[tauri::command]
fn save_config(config: WallisConfig, state: State<'_, AppState>) -> Result<WallisConfig, String> {
    if !config.persona_name.starts_with("Wallis") { return Err("Persona name must start with Wallis.".into()); }
    if !config.rootfs_sha256.trim().is_empty() { validate_sha256(&config.rootfs_sha256)?; }
    *state.config.lock().expect("config state poisoned") = config.clone();
    Ok(config)
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
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default().manage(AppState::default()).invoke_handler(tauri::generate_handler![get_setup_status, get_runtime_status, import_wallis, start_hermes, stop_hermes, reset_wallis, get_config, save_config]).run(tauri::generate_context!()).expect("error while running Wallis");
}
