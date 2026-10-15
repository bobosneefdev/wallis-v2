#![cfg(windows)]
use serde_json::json;
use std::process::Command;
use wallis_core::{jobs::Jobs, settings::{Roles, WallisConfig}, wsl::Session};

#[test]
#[ignore = "requires a disposable Wallis WSL2 distro imported by the CI workflow"]
fn manager_migrates_native_data_and_preserves_runtime_changes() {
    assert_eq!(std::env::var("WALLIS_WSL_TEST").as_deref(), Ok("1"));
    let data = std::env::temp_dir().join(format!("wallis-wsl-test-{}", std::process::id()));
    std::fs::create_dir_all(data.join("memory")).unwrap();
    std::fs::create_dir_all(data.join("pi/worker")).unwrap();
    std::fs::write(data.join("memory/probe"), "native-memory").unwrap();
    std::fs::write(data.join("roles.json"), serde_json::to_vec(&Roles::default()).unwrap()).unwrap();
    std::fs::write(data.join("profile.json"), r#"{"name":"Wallis Test"}"#).unwrap();
    std::fs::write(data.join("pi/worker/auth.json"), r#"{"probe":"desktop"}"#).unwrap();
    Jobs::open(data.join("jobs.sqlite").to_str().unwrap()).unwrap().enqueue_scoped("native-job", "10", 1, 2).unwrap();
    let start = || Session::start(&data, WallisConfig::default(), "0.4.0", &|_| {}).unwrap_or_else(|error| {
        panic!("{error}\n{}", std::fs::read_to_string(data.join("runtime.log")).unwrap_or_default())
    });
    let mut session = start();
    assert_eq!(session.request("get_profile", json!({})).unwrap()["name"], "Wallis Test");
    assert_eq!(session.request("get_jobs", json!({})).unwrap()[0]["task"], "native-job");
    let memory = Command::new("wsl.exe").args(["-d", "Wallis", "-u", "wallis", "--", "cat", "/var/lib/wallis/memory/probe"]).output().unwrap();
    assert!(memory.status.success());
    assert_eq!(memory.stdout, b"native-memory");
    let mods = session.request("get_mods", json!({})).unwrap();
    assert_eq!(mods.as_array().unwrap().len(), 6);
    session.request("set_mod_enabled", json!({"name":"volume","enabled":false})).unwrap();
    session.request("delete_mod", json!({"name":"stop"})).unwrap();
    // A failed second connection must not terminate the manager that owns the distro.
    assert!(Session::start(&data, WallisConfig::default(), "0.4.0", &|_| {}).is_err());
    assert_eq!(session.request("get_runtime_status", json!({})).unwrap()["state"], "stopped");
    // Simulate a provider refresh without syncing it back before closing the manager.
    let refreshed = Command::new("wsl.exe").args(["-d", "Wallis", "-u", "worker", "--", "python3", "-c", "from pathlib import Path; Path('/var/lib/wallis/pi/worker/auth.json').write_text('{\"probe\":\"linux\"}')"]).status().unwrap();
    assert!(refreshed.success());
    drop(session);
    let mut session = start();
    let mods = session.request("get_mods", json!({})).unwrap();
    let mods = mods.as_array().unwrap();
    assert!(!mods.iter().any(|m| m["name"] == "stop"));
    assert_eq!(mods.iter().find(|m| m["name"] == "volume").unwrap()["enabled"], false);
    let auth: serde_json::Value = serde_json::from_slice(&std::fs::read(data.join("pi/worker/auth.json")).unwrap()).unwrap();
    assert_eq!(auth["probe"], "linux");
    assert_eq!(session.request("get_jobs", json!({})).unwrap().as_array().unwrap().len(), 1);
    drop(session);
    assert!(data.join("memory/probe").exists(), "migration preserves the native copy");
    std::fs::remove_dir_all(data).unwrap();
}
