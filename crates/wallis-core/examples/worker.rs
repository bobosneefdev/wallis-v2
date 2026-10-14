//! Windows check: `worker <data_dir>` starts the confined worker Pi through `Agent::spawn` and sends one prompt.
//! Without credentials Pi must answer with an error (proving RPC works inside the sandbox), not die.
#[cfg(windows)]
fn main() -> Result<(), String> {
    use std::time::Duration;
    use wallis_core::agent::{Access, Agent};
    use wallis_core::provision::{self, Layout};
    use wallis_core::role::{Role, RoleConfig};
    let data = std::path::PathBuf::from(std::env::args().nth(1).ok_or("usage: worker <data_dir>")?);
    let layout = Layout(data.clone());
    provision::install(&provision::PI, &layout.tools(), "pi.exe")?;
    provision::download_verified(provision::BUSYBOX_URL, provision::BUSYBOX_SHA256, &layout.shell())?;
    let (scratch, dir) = (data.join("worker-scratch"), data.join("pi").join("worker"));
    for path in [&scratch, &dir] { std::fs::create_dir_all(path).map_err(|e| e.to_string())?; }
    std::fs::write(dir.join("settings.json"), serde_json::json!({ "shellPath": layout.shell() }).to_string()).map_err(|e| e.to_string())?;
    let access = Access { read: vec![layout.tools()], write: vec![scratch.clone(), dir.clone()], path: vec![layout.shell().parent().unwrap().into()] };
    let config = RoleConfig { provider: "openai-codex".into(), model: "gpt-5.5".into(), ..Default::default() };
    let mut agent = Agent::spawn(&layout.pi().to_string_lossy(), Role::Worker, &config, &dir, "You are a test worker.", &scratch, Some(&access))?;
    println!("worker spawned");
    println!("turn result: {:?}", agent.turn("Say hi.", Duration::from_secs(60), &mut |_| "no host".into(), &mut |_| {}));
    Ok(())
}
#[cfg(not(windows))]
fn main() {}
