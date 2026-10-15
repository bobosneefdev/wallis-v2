//! Validated slash-command mods. Their only Discord access is the host action protocol.
use crate::agent::Access;
use crate::pi::HostCall;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OptionSpec { pub name: String, pub description: String, #[serde(rename = "type")] pub kind: String, #[serde(default)] pub required: bool }
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Mod { pub name: String, pub description: String, #[serde(default)] pub options: Vec<OptionSpec>, #[serde(default = "yes")] pub enabled: bool }
fn yes() -> bool { true }
pub fn valid_name(name: &str) -> bool { (1..=32).contains(&name.len()) && name.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-') }
impl Mod {
    pub fn validate(&self) -> Result<(), String> {
        let description = |s: &str| (1..=100).contains(&s.chars().count()) && !s.chars().any(char::is_control);
        if !valid_name(&self.name) || self.name == "profile" || !description(&self.description) || self.options.len() > 25 { return Err("Invalid command name, description or option count.".into()); }
        let (mut names, mut optional) = (BTreeSet::new(), false);
        for o in &self.options {
            if !valid_name(&o.name) || !names.insert(&o.name) || !description(&o.description) || !matches!(o.kind.as_str(), "string" | "integer" | "boolean") || optional && o.required { return Err("Invalid, duplicate or incorrectly ordered command options.".into()); }
            optional |= !o.required;
        }
        Ok(())
    }
    pub fn validate_input(&self, input: &Value) -> Result<(), String> {
        let options = input["options"].as_object().ok_or("Options must be an object.")?;
        for (name, value) in options {
            let o = self.options.iter().find(|o| &o.name == name).ok_or("Unknown command option.")?;
            if !match o.kind.as_str() { "string" => value.as_str().is_some_and(|v| v.len() <= 6000), "integer" => value.as_i64().is_some(), "boolean" => value.is_boolean(), _ => false } { return Err(format!("Invalid value for {name}.")); }
        }
        if self.options.iter().any(|o| o.required && !options.contains_key(&o.name)) { return Err("Missing required command option.".into()); }
        Ok(())
    }
}

pub fn scan(data: &Path) -> Result<Vec<Mod>, String> {
    let states: BTreeMap<String, bool> = match std::fs::read(data.join("mods.json")) { Ok(b) => serde_json::from_slice(&b).map_err(|e| format!("Invalid mods.json: {e}"))?, Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(), Err(e) => return Err(e.to_string()) };
    let mut mods = Vec::new();
    for entry in std::fs::read_dir(data.join("mods")).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        if !entry.file_type().map_err(|e| e.to_string())?.is_dir() { continue; }
        let name = entry.file_name().to_string_lossy().into_owned();
        if !valid_name(&name) || !entry.path().join("mod.json").is_file() { continue; }
        let parsed = (|| {
            let mut m: Mod = serde_json::from_slice(&std::fs::read(confined_path(&data.join("mods"), &entry.path().join("mod.json"))?).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
            m.validate()?;
            if m.name != name || !confined_path(&data.join("mods"), &entry.path().join("index.ts"))?.is_file() { return Err("Mod directory and manifest must agree and contain index.ts.".into()); }
            m.enabled = states.get(&m.name).copied().unwrap_or(true);
            Ok::<_, String>(m)
        })();
        match parsed { Ok(m) => mods.push(m), Err(e) => eprintln!("wallis: ignoring mod {name}: {e}") }
    }
    mods.sort_by(|a,b| a.name.cmp(&b.name));
    if mods.len() > 99 { return Err("Discord allows at most 99 mods plus /profile.".into()); }
    Ok(mods)
}

pub fn confined_path(root: &Path, path: &Path) -> Result<PathBuf, String> {
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let path = if path.is_absolute() { path.to_path_buf() } else { root.join(path) }.canonicalize().map_err(|e| e.to_string())?;
    if !path.starts_with(&root) || path == root { return Err("Path is outside the permitted folder.".into()); }
    Ok(path)
}

pub fn check_budget(data: &Path) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        // The fixed ext4 filesystem enforces the cap, including private browser
        // and provider folders the bot must not traverse as another uid.
        let space = std::process::Command::new("/usr/bin/stat").args(["--file-system", "--format=%a", "--"]).arg(data.join("workspace")).output().map_err(|e| format!("Unable to check workspace space: {e}"))?;
        if !space.status.success() { return Err(format!("Unable to check workspace space: {}", String::from_utf8_lossy(&space.stderr).trim())); }
        let available = String::from_utf8_lossy(&space.stdout).trim().parse::<u64>().map_err(|e| format!("Invalid workspace space: {e}"))?;
        if available == 0 { return Err("The 3 GiB workspace budget is exhausted. Delete unused downloads and mods.".into()); }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = data; // Workers and mods cannot run outside the Linux runtime.
    Ok(())
}

pub fn action(line: &str) -> Result<HostCall, String> {
    let value: Value = serde_json::from_str(line).map_err(|e| e.to_string())?;
    let tool = value["tool"].as_str().ok_or("Missing action tool.")?;
    if !matches!(tool, "queue" | "queue_list" | "skip_audio" | "stop_audio" | "volume" | "message" | "file" | "delegate" | "reply") || !value["args"].is_object() { return Err("This action is not available to mods.".into()); }
    Ok(HostCall { request_id: String::new(), tool: tool.into(), args: value["args"].clone() })
}

pub fn run(data: &Path, name: &str, input: Value, on_action: &mut dyn FnMut(HostCall) -> String) -> Result<String, String> {
    check_budget(data)?;
    let m = scan(data)?.into_iter().find(|m| m.name == name && m.enabled).ok_or("Unknown or disabled mod.")?;
    m.validate_input(&input)?;
    let mods = data.join("mods");
    let access = Access { read: vec![mods.clone()], write: vec![data.join("media"), data.join("worker-scratch")], path: vec![] };
    let env = vec![("WALLIS_MEDIA".into(), data.join("media").to_string_lossy().into())];
    confined_path(&mods, &mods.join(name).join("index.ts"))?;
    let args = vec![mods.join(name).join("index.ts").to_string_lossy().into_owned()];
    let mut child = crate::sandbox::command(Path::new("/opt/wallis/tools/bun/bun"), &args, &mods, &env, &access, true)?.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit()).spawn().map_err(|e| e.to_string())?;
    let result = (|| {
        let mut stdin = child.stdin.take().ok_or("Mod stdin unavailable.")?;
        writeln!(stdin, "{input}").map_err(|e| e.to_string())?;
        let stdout = child.stdout.take().ok_or("Mod stdout unavailable.")?;
        let (tx, rx) = mpsc::sync_channel(8);
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut line = String::new();
                // Bound each protocol frame before allocating unbounded memory.
                match Read::by_ref(&mut reader).take(65537).read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) if line.len() <= 65536 && line.ends_with('\n') => if tx.send(Ok(line)).is_err() { break },
                    _ => { let _ = tx.send(Err("Invalid or oversized mod frame.".to_string())); break; }
                }
            }
        });
        let (started, mut reply, mut count) = (Instant::now(), String::new(), 0);
        let mut failure = None;
        loop {
            check_budget(data)?;
            if started.elapsed() > Duration::from_secs(300) { return Err("Mod timed out.".into()); }
            match rx.recv_timeout(Duration::from_millis(250)) {
                Ok(line) => {
                    count += 1; if count > 100 { return Err("Too many mod actions.".into()); }
                    let call = action(&line?)?;
                    let result = if call.tool == "reply" { reply = call.args["text"].as_str().unwrap_or("").chars().take(1900).collect(); "Replied.".into() } else { on_action(call) };
                    if result.starts_with("Failed:") { failure = Some(result.clone()); }
                    writeln!(stdin, "{}", json!({"result": result})).map_err(|e| e.to_string())?;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        if !child.wait().map_err(|e| e.to_string())?.success() { return Err("Mod exited unsuccessfully; see the runtime log.".into()); }
        if let Some(error) = failure { return Err(error); }
        Ok(if reply.is_empty() { "Done.".into() } else { reply })
    })();
    crate::sandbox::kill(&mut child, true);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    #[test]
    fn budget_checks_the_filesystem_without_reading_private_worker_files() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("wallis-budget-{}", std::process::id()));
        std::fs::create_dir_all(root.join("workspace")).unwrap();
        let private = root.join("worker-scratch/private");
        std::fs::create_dir_all(&private).unwrap();
        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o000)).unwrap();
        let result = check_budget(&root);
        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::remove_dir_all(root).unwrap();
        assert!(result.is_ok(), "{result:?}");
    }
    #[test]
    fn validates_manifests_inputs_actions_and_paths() {
        let mut m: Mod = serde_json::from_value(json!({"name":"play","description":"Play music","options":[{"name":"query","description":"Song","type":"string","required":true}]})).unwrap();
        assert!(m.validate().is_ok());
        assert!(m.validate_input(&json!({"options":{"query":"hi"}})).is_ok());
        assert!(m.validate_input(&json!({"options":{"query":3}})).is_err());
        assert!(m.validate_input(&json!({"options":{}})).is_err());
        m.options.push(m.options[0].clone()); assert!(m.validate().is_err());
        m.name = "../escape".into(); assert!(m.validate().is_err());
        assert!(action(r#"{"tool":"queue","args":{"path":"a"}}"#).is_ok());
        assert!(action(r#"{"tool":"run_mod","args":{}}"#).is_err());
        let root = std::env::temp_dir().join(format!("wallis-path-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap(); std::fs::write(root.join("ok"), "x").unwrap();
        assert!(confined_path(&root, Path::new("ok")).is_ok());
        assert!(confined_path(&root, &root.join("..")).is_err());
        #[cfg(unix)] { std::os::unix::fs::symlink("/etc/passwd", root.join("escape")).unwrap(); assert!(confined_path(&root, Path::new("escape")).is_err()); }
        std::fs::remove_dir_all(root).unwrap();
    }
}
