//! Private stdio control connection from the desktop. The Discord token exists only in process memory.
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use wallis_core::{mods, prompts, settings::*, wsl::Bootstrap};
use wallis_core::provision::{Layout, SpeechServers};
use wallis_core::jobs::Jobs;
use wallis_core::participation::Participation;
use wallis_discord::bot::{self, Bot, BotConfig};

const DATA: &str = "/var/lib/wallis";
struct Running { bot: Arc<Mutex<Option<Bot>>>, out: Arc<Mutex<Option<wallis_discord::Out>>>, failure: Arc<Mutex<Option<String>>>, _runtime: tokio::runtime::Runtime, _speech: SpeechServers }
impl Drop for Running { fn drop(&mut self) { self.bot.lock().unwrap().take(); } }
struct Server { init: Bootstrap, running: Option<Running> }

#[cfg(target_os = "linux")]
fn prepare(init: &Bootstrap) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    if unsafe { libc::geteuid() } != 0 { return Err("Start the control server through the Wallis distro launcher.".into()); }
    let data = Path::new(DATA);
    std::fs::create_dir_all(data).map_err(|e| e.to_string())?;
    let workspace = data.join("workspace");
    std::fs::create_dir_all(&workspace).map_err(|e| e.to_string())?;
    let image = data.join("workspace.img");
    if !image.exists() {
        let file = std::fs::OpenOptions::new().write(true).create_new(true).open(&image).map_err(|e|e.to_string())?;
        file.set_len(3 * 1024 * 1024 * 1024).map_err(|e|e.to_string())?;
        std::fs::set_permissions(&image,std::fs::Permissions::from_mode(0o600)).map_err(|e|e.to_string())?;
        if !std::process::Command::new("mkfs.ext4").args(["-q","-F","-m","0"]).arg(&image).status().map_err(|e|e.to_string())?.success() { return Err("Unable to create the bounded workspace.".into()); }
    }
    if !std::process::Command::new("mountpoint").arg("-q").arg(&workspace).status().map_err(|e|e.to_string())?.success() && !std::process::Command::new("mount").args(["-o","loop,nosuid,nodev"]).arg(&image).arg(&workspace).status().map_err(|e|e.to_string())?.success() { return Err("Unable to mount the 3 GiB workspace.".into()); }
    for name in ["mods", "media", "worker-scratch"] {
        std::fs::create_dir_all(workspace.join(name)).map_err(|e|e.to_string())?;
        let alias = data.join(name);
        if !alias.exists() { std::os::unix::fs::symlink(workspace.join(name),&alias).map_err(|e|e.to_string())?; }
        if alias.canonicalize().map_err(|e|e.to_string())? != workspace.join(name) { return Err("Unexpected workspace path.".into()); }
    }
    for name in ["pi/realtime", "pi/worker", "memory", "realtime-cwd", "logs"] {
        std::fs::create_dir_all(data.join(name)).map_err(|e| e.to_string())?;
        for component in [data.join(name), data.join(name).parent().unwrap().into()] {
            if std::fs::symlink_metadata(&component).map_err(|e|e.to_string())?.is_symlink() { return Err("Runtime directories cannot be symlinks.".into()); }
        }
        std::fs::set_permissions(data.join(name), std::fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
    }
    let write = |path: &Path, body: &str| -> Result<(),String> {
        let temporary = path.with_extension(format!("{}-bootstrap",std::process::id()));
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&temporary).map_err(|e|e.to_string())?;
        file.write_all(body.as_bytes()).map_err(|e|e.to_string())?;
        std::fs::rename(temporary,path).map_err(|e|e.to_string())
    };
    // Only known desktop-owned configuration files cross this boundary. Never overwrite Linux's fresher OAuth refresh with an unchanged desktop copy.
    for (name, body) in &init.files {
        if !wallis_core::wsl::SYNC_FILES.contains(&name.as_str()) { return Err("Unexpected bootstrap file.".into()); }
        if name == "profile.json" && data.join(name).exists() { continue; }
        let marker = data.join(format!(".source-{}", name.replace('/', "-")));
        let hash = wallis_core::provision::sha256_hex(body.as_bytes());
        if std::fs::read_to_string(&marker).ok().as_deref() != Some(&hash) {
            write(&data.join(name), body)?;
            std::fs::write(marker, hash).map_err(|e| e.to_string())?;
        }
    }
    for (name, role) in [("realtime", &init.roles.realtime), ("worker", &init.roles.worker)] {
        let dir = data.join("pi").join(name);
        write(&dir.join("settings.json"), &json!({"shellPath":"/bin/bash","packages":role.packages}).to_string())?;
        write(&dir.join("wallis.ts"), wallis_core::pi::EXTENSION)?;
    }
    // Seed only once: deleted or customized stock commands must stay that way.
    if !data.join(".mods-seeded").exists() {
        let source = Path::new("/opt/wallis/stock-mods");
        let status = std::process::Command::new("cp").args(["-a", "--no-clobber"]).arg(source.join(".")).arg(data.join("mods")).status().map_err(|e| e.to_string())?;
        if !status.success() { return Err("Unable to seed stock mods.".into()); }
        let dependencies = data.join("mods/node_modules");
        if std::fs::symlink_metadata(&dependencies).is_ok_and(|m|m.is_symlink()) { std::fs::remove_file(&dependencies).map_err(|e|e.to_string())?; }
        if !dependencies.exists() {
            let status = std::process::Command::new("cp").arg("-a").arg("/opt/wallis/tools/node/node_modules").arg(&dependencies).status().map_err(|e|e.to_string())?;
            if !status.success() { return Err("Unable to seed mod dependencies.".into()); }
        }
        std::fs::write(data.join(".mods-seeded"), "1").map_err(|e| e.to_string())?;
    }
    for name in ["tools", "models"] {
        let target = data.join(name);
        if !target.exists() { std::os::unix::fs::symlink(format!("/opt/wallis/{name}"), target).map_err(|e| e.to_string())?; }
    }
    let chown = |owner: &str, path: &Path| -> Result<(), String> {
        let status = std::process::Command::new("chown").args(["-R", owner]).arg(path).status().map_err(|e| e.to_string())?;
        if status.success() { Ok(()) } else { Err("Unable to set runtime ownership.".into()) }
    };
    // chown does not traverse the tools/models symlinks.
    chown("wallis:wallis", data)?;
    for name in ["pi/worker", "mods", "media", "worker-scratch"] { chown("worker:worker", &data.join(name).canonicalize().map_err(|e|e.to_string())?)?; }
    // Songbird runs as wallis and must read and delete downloads owned by worker.
    chown("worker:wallis", &data.join("media").canonicalize().map_err(|e|e.to_string())?)?;
    std::fs::set_permissions(data.join("media"), std::fs::Permissions::from_mode(0o2770)).map_err(|e|e.to_string())?;
    for name in ["pi/realtime", "pi/worker"] { std::fs::set_permissions(data.join(name), std::fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?; }
    // Clean stale partials after a crash. New downloads are owned by the worker.
    for entry in std::fs::read_dir(data.join("media")).map_err(|e| e.to_string())? { let p = entry.map_err(|e| e.to_string())?.path(); if p.is_dir() { std::fs::remove_dir_all(p) } else { std::fs::remove_file(p) }.map_err(|e| e.to_string())?; }
    // Drop every root capability before starting speech, Pi or Discord.
    if unsafe { libc::setgroups(0, std::ptr::null()) } != 0 || unsafe { libc::setgid(1000) } != 0 || unsafe { libc::setuid(1000) } != 0 { return Err("Unable to drop runtime privileges.".into()); }
    std::env::set_var("HOME", "/home/wallis");
    std::env::set_var("PATH", "/opt/wallis/tools/bun:/opt/wallis/tools/pi:/opt/wallis/tools/node/node_modules/.bin:/usr/local/bin:/usr/bin:/bin");
    Ok(())
}
#[cfg(not(target_os = "linux"))]
fn prepare(_: &Bootstrap) -> Result<(), String> { Err("wallis-bot requires Linux.".into()) }

impl Server {
    fn config(&self) -> Result<BotConfig, String> {
        let c = &self.init.config;
        let mode = |s: &str| if s == "mentions" { Participation::Mentions } else { Participation::Natural };
        let list = |s: &str| s.split(',').filter(|s| !s.is_empty()).map(String::from).collect();
        let data = PathBuf::from(DATA); let layout = Layout(data.clone());
        for role in [&self.init.roles.realtime, &self.init.roles.worker] { role.validate()?; }
        Ok(BotConfig { pi_program: layout.pi().to_string_lossy().into(), worker_program: layout.pi().to_string_lossy().into(), data_dir: data.clone(), persona: persona(&data), realtime: self.init.roles.realtime.clone(), worker: self.init.roles.worker.clone(), write_channels: list(&c.discord_write_channels), voice_text_channel: c.discord_voice_text_channel.clone(), python: layout.python().to_string_lossy().into(), voice_participation: mode(&c.voice_participation), text_participation: mode(&c.text_participation), cooldown: Duration::from_secs(c.participation_cooldown_secs.into()), speech_speed: c.tts_speed, sandbox_read: vec![layout.tools()], shell: layout.shell() })
    }
    fn start(&mut self, token: &str) -> Result<Value, String> {
        if self.running.is_some() { return self.handle("get_runtime_status", &json!({})); }
        if token.is_empty() { return Err("Save the Discord token first.".into()); }
        read_profile(Path::new(DATA))?;
        let config = self.config()?;
        let c = &self.init.config;
        let speech = SpeechServers::start(&Layout(DATA.into()), &c.stt_model, c.stt_threads, &c.tts_voice)?;
        let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
        let (bot, out, failure) = (Arc::new(Mutex::new(None::<Bot>)), Arc::new(Mutex::new(None)), Arc::new(Mutex::new(None)));
        let (ready_bot, event_bot, ready_out, startup_failure, end_failure) = (bot.clone(), bot.clone(), out.clone(), failure.clone(), failure.clone());
        let list = |s: &str| s.split(',').filter(|s| !s.is_empty()).map(String::from).collect();
        let settings = wallis_discord::Settings { token: token.into(), data_dir: DATA.into(), allowed_users: list(&c.discord_allowed_users), read_channels: list(&c.discord_read_channels), write_channels: list(&c.discord_write_channels), profile: Arc::new(|| profile_json(Path::new(DATA))) };
        runtime.spawn(async move {
            let ready = move |out: wallis_discord::Out| { *ready_out.lock().unwrap() = Some(out.clone()); std::thread::spawn(move || match bot::start(config, out) { Ok(started) => *ready_bot.lock().unwrap() = Some(started), Err(e) => *startup_failure.lock().unwrap() = Some(e) }); };
            let event = move |event| { if let Some(bot) = event_bot.lock().unwrap().as_ref() { let _ = bot.sender.send(event); } };
            if let Err(e) = wallis_discord::run(settings, ready, event).await { *end_failure.lock().unwrap() = Some(e); }
        });
        self.running = Some(Running { bot, out, failure, _runtime: runtime, _speech: speech });
        Ok(json!({"state":"running","message":"Wallis is connecting to Discord…"}))
    }
    fn refresh_commands(&self) -> Result<(), String> {
        if let Some(r) = &self.running { if let Some(out) = r.out.lock().unwrap().as_ref() { out.set_commands()?; } }
        Ok(())
    }
    fn handle(&mut self, command: &str, args: &Value) -> Result<Value, String> {
        let data = Path::new(DATA);
        match command {
            "start_wallis" => self.start(args["token"].as_str().unwrap_or("")),
            "stop_wallis" => { self.running.take(); Ok(json!({"state":"stopped","message":"Wallis is stopped."})) }
            "get_runtime_status" => Ok(match &self.running { None => json!({"state":"stopped","message":"Wallis is stopped."}), Some(r) => match r.failure.lock().unwrap().as_ref() { Some(e) => json!({"state":"unavailable","message":e}), None => json!({"state":"running","message":"Wallis is online."}) } }),
            "get_stats" => Ok(self.running.as_ref().and_then(|r| r.bot.lock().unwrap().as_ref().map(|b| json!(b.stats.snapshot()))).unwrap_or(Value::Null)),
            "get_audit_log" => Ok(self.running.as_ref().and_then(|r| r.bot.lock().unwrap().as_ref().map(|b| json!(b.stats.audit_log()))).unwrap_or(json!([]))),
            "get_profile" => serde_json::to_value(read_profile(data)?).map_err(|e| e.to_string()),
            "get_jobs" => Ok(json!(Jobs::open(&data.join("jobs.sqlite").to_string_lossy())?.with_status(&["queued","running","done","failed"])?)),
            "retry_job" => Ok(json!(Jobs::open(&data.join("jobs.sqlite").to_string_lossy())?.retry(args["id"].as_i64().ok_or("Missing job id.")?)?)),
            "get_mods" => Ok(json!(mods::scan(data)?)),
            "set_mod_enabled" | "delete_mod" => {
                let name = args["name"].as_str().ok_or("Missing command name.")?;
                if !mods::valid_name(name) || name == "profile" { return Err("Invalid command name.".into()); }
                if command == "delete_mod" {
                    let path = mods::confined_path(&data.join("mods"), &data.join("mods").join(name))?;
                    let status = std::process::Command::new("sudo").args(["-n","-u","worker","--","rm","-rf","--"]).arg(path).status().map_err(|e| e.to_string())?;
                    if !status.success() { return Err("Unable to delete command.".into()); }
                }
                let path = data.join("mods.json");
                let mut states: std::collections::BTreeMap<String,bool> = if path.exists() { serde_json::from_slice(&std::fs::read(&path).map_err(|e|e.to_string())?).map_err(|e|e.to_string())? } else { Default::default() };
                // Keep a tombstone: recreating the directory cannot silently re-enable a deleted command.
                states.insert(name.into(), if command == "delete_mod" { false } else { args["enabled"].as_bool().ok_or("Missing enabled state.")? });
                std::fs::write(path.with_extension("tmp"), json!(states).to_string()).and_then(|_| std::fs::rename(path.with_extension("tmp"), &path)).map_err(|e|e.to_string())?;
                self.refresh_commands()?; Ok(json!(mods::scan(data)?))
            }
            "generate_wallis" => {
                if read_profile(data).is_ok() { return Err("Wallis already has a profile. Only he can change it.".into()); }
                let config = self.config()?;
                let mut agent = bot::spawn_realtime_with(&config, &prompts::profile_generator_prompt())?;
                let answer = agent.turn(&generation_prompt(args["hint"].as_str().unwrap_or("")), Duration::from_secs(120), &mut |_| "Unavailable during generation.".into(), &mut |_| {})?;
                let profile = sanitize_profile(serde_json::from_str(extract_json(&answer).ok_or("The model did not return a profile.")?).map_err(|e|e.to_string())?);
                let stored = json!(profile).to_string();
                std::fs::write(data.join("profile.json"), &stored).map_err(|e| e.to_string())?;
                agent.turn(&format!("The profile was saved. Store every detail as durable facts about Wallis, then reply done.\n{stored}"), Duration::from_secs(120), &mut |_| "Unavailable.".into(), &mut |_| {})?;
                Ok(json!(profile))
            }
            "lobotomize_wallis" => {
                self.running.take();
                for path in [data.join("memory"), data.join("profile.json")] {
                    let result = if path.is_dir() { std::fs::remove_dir_all(path) } else { std::fs::remove_file(path) };
                    if let Err(e) = result { if e.kind() != std::io::ErrorKind::NotFound { return Err(e.to_string()); } }
                }
                Ok(Value::Null)
            }
            _ => Err("Unknown control command.".into()),
        }
    }
    fn files(&self) -> std::collections::BTreeMap<String,String> {
        wallis_core::wsl::SYNC_FILES.iter().filter_map(|name| {
            let path = Path::new(DATA).join(name);
            let content = if name.starts_with("pi/worker/") { std::process::Command::new("sudo").args(["-n","-u","worker","--","cat","--"]).arg(path).output().ok().filter(|o|o.status.success()).and_then(|o|String::from_utf8(o.stdout).ok()) } else { std::fs::read_to_string(path).ok() };
            content.map(|text| (name.to_string(), text))
        }).collect()
    }
}

fn self_check() -> Result<(),String> {
    let init = Bootstrap { config: WallisConfig::default(), roles: Roles::default(), files: Default::default() };
    prepare(&init)?;
    let data = Path::new(DATA);
    std::fs::write(data.join("pi/realtime/secret-probe"),"private").map_err(|e|e.to_string())?;
    let status = std::process::Command::new("sudo").args(["-n","-u","worker","--","touch","/var/lib/wallis/pi/worker/secret-probe"]).status().map_err(|e|e.to_string())?;
    if !status.success() { return Err("Unable to create worker isolation probe.".into()); }
    let access = wallis_core::agent::Access { read: vec![], write: vec![data.join("worker-scratch"),data.join("media")], path:vec![] };
    let script = "set -eu; test ! -r /var/lib/wallis/pi/realtime/secret-probe; test ! -r /var/lib/wallis/pi/worker/secret-probe; test ! -w /opt/wallis/bin/wallis-bot; test ! -e /mnt/c; touch /var/lib/wallis/media/probe; agent-browser --session self-check open 'data:text/html,<title>Wallis</title>'; agent-browser --session self-check get title | grep -Fx Wallis; agent-browser --session self-check close";
    let output = wallis_core::sandbox::command(Path::new("/bin/bash"), &["-c".into(),script.into()], &data.join("worker-scratch"), &[], &access,true)?.output().map_err(|e|e.to_string())?;
    if !output.status.success() { return Err(format!("Isolation check failed: {}",String::from_utf8_lossy(&output.stderr))); }
    std::fs::remove_file(data.join("media/probe")).map_err(|e|format!("The bot cannot clean downloaded media: {e}"))?;
    let mut actions = Vec::new();
    let reply = mods::run(data,"volume",json!({"options":{"percent":40},"guild":"1","user":"2","channel":"3"}), &mut |call| { actions.push((call.tool,call.args)); "Volume changed.".into() })?;
    if actions != vec![("volume".into(),json!({"percent":40}))] || reply != "Volume changed." { return Err("Mod protocol roundtrip failed.".into()); }
    std::fs::remove_file(data.join("pi/realtime/secret-probe")).map_err(|e|e.to_string())?;
    let _ = std::process::Command::new("sudo").args(["-n","-u","worker","--","rm","-f","/var/lib/wallis/pi/worker/secret-probe"]).status();
    println!("Isolation and stock mod roundtrip passed.");
    Ok(())
}

fn main() {
    if let Err(error) = serve() { eprintln!("wallis: {error}"); std::process::exit(1); }
}
fn serve() -> Result<(), String> {
    if std::env::args().any(|arg|arg == "--self-check") { return self_check(); }
    if std::env::args().any(|arg| arg == "--provision") {
        let layout = Layout("/opt/wallis".into());
        for voice in ["alba","marius","javert"] { wallis_core::provision::ensure_all(&layout,"parakeet-0.6b-v3-int8",voice,&|s| eprintln!("{s}"))?; }
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    let _lock = {
        use std::os::fd::AsRawFd;
        let lock = std::fs::OpenOptions::new().write(true).create(true).truncate(false).open("/run/wallis.lock").map_err(|e|e.to_string())?;
        if unsafe { libc::flock(lock.as_raw_fd(),libc::LOCK_EX | libc::LOCK_NB) } != 0 { return Err("Another Wallis manager is already connected.".into()); }
        lock
    };
    println!("{}", json!({"claimed":true})); std::io::stdout().flush().map_err(|e|e.to_string())?;
    let stdin = std::io::stdin(); let mut lines = stdin.lock().lines();
    let init: Bootstrap = serde_json::from_str(&lines.next().ok_or("Missing bootstrap.")?.map_err(|e|e.to_string())?).map_err(|e| e.to_string())?;
    prepare(&init)?;
    for (name,role) in [("realtime",&init.roles.realtime),("worker",&init.roles.worker)] {
        let dir = Path::new(DATA).join("pi").join(name);
        if name == "realtime" { wallis_core::pi_config::install_packages("/opt/wallis/tools/pi/pi",&dir,&role.packages)?; }
        else {
            let access = wallis_core::agent::Access { read:vec![], write:vec![dir.clone(),Path::new(DATA).join("worker-scratch"),Path::new(DATA).join("mods"),Path::new(DATA).join("media")],path:vec![] };
            for package in &role.packages {
                wallis_core::pi_config::valid_package(package)?;
                let env = vec![("PI_CODING_AGENT_DIR".into(),dir.to_string_lossy().into())];
                let status = wallis_core::sandbox::command(Path::new("/opt/wallis/tools/pi/pi"),&["install".into(),package.clone()],&Path::new(DATA).join("worker-scratch"),&env,&access,false)?.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::inherit()).status().map_err(|e|e.to_string())?;
                if !status.success() { return Err(format!("Unable to install worker package {package}.")); }
            }
        }
    }
    let mut server = Server { init, running: None };
    println!("{}", json!({"ready":true})); std::io::stdout().flush().map_err(|e|e.to_string())?;
    for line in lines {
        let request: Value = serde_json::from_str(&line.map_err(|e|e.to_string())?).map_err(|e|e.to_string())?;
        let result = server.handle(request["command"].as_str().unwrap_or(""), &request["args"]);
        let response = match result { Ok(value) => json!({"result":value,"files":server.files()}), Err(error) => json!({"error":error,"files":server.files()}) };
        println!("{response}"); std::io::stdout().flush().map_err(|e|e.to_string())?;
    }
    // Exiting kills detached brain/worker threads and their distro process tree through WSL termination.
    Ok(())
}
