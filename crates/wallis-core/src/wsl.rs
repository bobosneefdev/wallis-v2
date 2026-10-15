//! Windows manager for the isolated Linux runtime. No shell interpolation or network control port.
use crate::settings::{Roles, WallisConfig};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};

pub const SYNC_FILES: &[&str] = &["profile.json", "pi/realtime/auth.json", "pi/realtime/models.json", "pi/worker/auth.json", "pi/worker/models.json"];
#[derive(Serialize, Deserialize)]
pub struct Bootstrap { pub config: WallisConfig, pub roles: Roles, pub files: BTreeMap<String,String> }
impl Bootstrap {
    pub fn read(data: &Path, config: WallisConfig) -> Result<Self, String> {
        let roles: Roles = serde_json::from_slice(&std::fs::read(data.join("roles.json")).map_err(|_| "Choose the realtime and worker models first.".to_string())?).map_err(|e|e.to_string())?;
        let files = SYNC_FILES.iter().filter_map(|name| std::fs::read_to_string(data.join(name)).ok().map(|s|(name.to_string(),s))).collect();
        Ok(Self { config, roles, files })
    }
}
fn wsl() -> Command {
    #[allow(unused_mut)]
    let mut c = Command::new("wsl.exe");
    #[cfg(windows)] { use std::os::windows::process::CommandExt; c.creation_flags(0x0800_0000); }
    c
}
fn run(args: &[&str]) -> Result<(), String> {
    let output = wsl().args(args).output().map_err(|e|format!("WSL2 is required. Run `wsl --install --no-distribution` as administrator, restart Windows, then retry. {e}"))?;
    if output.status.success() { Ok(()) } else { Err(format!("WSL failed: {}", decode(&output.stderr).trim())) }
}
fn decode(bytes: &[u8]) -> String {
    if bytes.contains(&0) { String::from_utf16_lossy(&bytes.chunks_exact(2).map(|c|u16::from_le_bytes([c[0],c[1]])).collect::<Vec<_>>()) } else { String::from_utf8_lossy(bytes).into() }
}
fn imported() -> Result<bool, String> {
    let out = wsl().args(["--list","--quiet"]).output().map_err(|e|format!("Install WSL2 with `wsl --install --no-distribution`, then restart Windows. {e}"))?;
    if !out.status.success() { return Err("WSL2 is unavailable. Run `wsl --install --no-distribution` as administrator and restart Windows.".into()); }
    Ok(decode(&out.stdout).lines().any(|s|s.trim() == "Wallis"))
}

pub fn ensure(data: &Path, version: &str, progress: &dyn Fn(&str)) -> Result<(), String> {
    if !cfg!(windows) { return Err("The desktop manager requires Windows and WSL2.".into()); }
    let exists = imported()?;
    if exists {
        let out = wsl().args(["-d","Wallis","-u","root","--","cat","/etc/wallis-release"]).output().map_err(|e|e.to_string())?;
        if out.status.success() && decode(&out.stdout).trim() == version { return Ok(()); }
    }
    progress("Downloading the Wallis Linux environment…");
    let release: Value = ureq::get(format!("https://api.github.com/repos/bobosneefdev/wallis-v2/releases/tags/v{version}")).header("Accept","application/vnd.github+json").call().map_err(|e|format!("Unable to find the Linux runtime release: {e}"))?.body_mut().read_json().map_err(|e|e.to_string())?;
    let asset = release["assets"].as_array().into_iter().flatten().find(|a|a["name"] == "wallis-rootfs.tar.gz").ok_or("Release has no Linux runtime.")?;
    let hash = asset["digest"].as_str().and_then(|s|s.strip_prefix("sha256:")).filter(|s|s.len()==64).ok_or("Runtime checksum missing.")?;
    let archive = data.join("downloads").join(format!("wallis-rootfs-{version}.tar.gz"));
    crate::provision::download_verified(asset["browser_download_url"].as_str().ok_or("Missing runtime URL.")?,hash,&archive)?;
    import_runtime(data, &archive, exists, progress)
}

fn import_runtime(data: &Path, archive: &Path, exists: bool, progress: &dyn Fn(&str)) -> Result<(), String> {
    let install = data.join("wsl"); std::fs::create_dir_all(&install).map_err(|e|e.to_string())?;
    let import = |archive: &Path| run(&["--import","Wallis",&install.to_string_lossy(),&archive.to_string_lossy(),"--version","2"]);
    if !exists {
        progress("Importing Wallis into WSL2…"); import(archive)?;
    } else {
        // Keep a full recoverable export before replacing a distro. Never delete the only copy of user data.
        progress("Backing up the existing Wallis environment…");
        let backup = data.join("wallis-before-upgrade.tar");
        if backup.exists() { return Err(format!("An earlier upgrade backup exists at {}. Move it aside after verifying the previous upgrade, then retry.",backup.display())); }
        run(&["--terminate","Wallis"])?;
        run(&["--export","Wallis",&backup.to_string_lossy()])?;
        let saved = data.join("wallis-user-data.tar");
        let output = std::fs::File::create(&saved).map_err(|e|e.to_string())?;
        let status = wsl().args(["-d","Wallis","-u","root","--","tar","--sparse","--one-file-system","-cf","-","-C","/","var/lib/wallis","home/wallis"]).stdout(output).status().map_err(|e|e.to_string())?;
        if !status.success() { return Err(format!("Unable to save runtime data. Full backup kept at {}.",backup.display())); }
        run(&["--terminate","Wallis"])?; run(&["--unregister","Wallis"])?;
        let upgrade = (|| {
            import(archive)?;
            let status = wsl().args(["-d","Wallis","-u","root","--","tar","-xf","-","-C","/"]).stdin(std::fs::File::open(&saved).map_err(|e|e.to_string())?).status().map_err(|e|e.to_string())?;
            if !status.success() { return Err("Unable to restore user data.".to_string()); }
            Ok(())
        })();
        if let Err(error) = upgrade {
            if imported()? { run(&["--unregister","Wallis"])?; }
            import(&backup).map_err(|e|format!("{error} Rollback failed: {e}. Restore {} with wsl --import.",backup.display()))?;
            return Err(format!("{error} The original distro was restored. Backup: {}",backup.display()));
        }
        std::fs::remove_file(saved).map_err(|e|e.to_string())?;
        // The full export remains available until the user verifies the upgrade.
    }
    progress(""); Ok(())
}

fn migrate_native(data: &Path) -> Result<(),String> {
    let needed = ["memory", "jobs.sqlite", "jobs.sqlite-wal", "jobs.sqlite-shm"].into_iter().filter(|name|data.join(name).exists()).collect::<Vec<_>>();
    if needed.is_empty() { return Ok(()); }
    if wsl().args(["-d","Wallis","-u","root","--","test","-e","/var/lib/wallis/.native-migrated"]).status().map_err(|e|e.to_string())?.success() { return Ok(()); }
    let archive = data.join("native-migration.tar");
    let status = Command::new("tar.exe").args(["-cf", &archive.to_string_lossy(), "-C", &data.to_string_lossy()]).args(needed).status().map_err(|e|e.to_string())?;
    if !status.success() { return Err("Unable to archive existing memories and jobs; the original files are unchanged.".into()); }
    let script = "import os,sys,tarfile; root='/var/lib/wallis'; os.makedirs(root,exist_ok=True); t=tarfile.open(fileobj=sys.stdin.buffer,mode='r|'); t.extractall(root,filter='data'); open(root+'/.native-migrated','w').write('1')";
    let status = wsl().args(["-d","Wallis","-u","root","--","python3","-c",script]).stdin(std::fs::File::open(&archive).map_err(|e|e.to_string())?).status().map_err(|e|e.to_string())?;
    if !status.success() { return Err("Unable to migrate existing memories. The original files are unchanged.".into()); }
    std::fs::remove_file(archive).map_err(|e|e.to_string())?;
    Ok(())
}

pub struct Session { child: Child, stdin: ChildStdin, lines: std::sync::mpsc::Receiver<Result<String,String>>, data: PathBuf, owned: bool }
impl Session {
    pub fn start(data: &Path, config: WallisConfig, version: &str, progress: &dyn Fn(&str)) -> Result<Self,String> {
        ensure(data,version,progress)?;
        migrate_native(data)?;
        let init = Bootstrap::read(data,config)?;
        let log = std::fs::File::create(data.join("runtime.log")).map_err(|e|e.to_string())?;
        let mut child = wsl().args(["-d","Wallis","-u","root","--","/opt/wallis/bin/wallis-bot"]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(log).spawn().map_err(|e|e.to_string())?;
        let stdin = child.stdin.take().ok_or("Missing runtime stdin.")?;
        let stdout = child.stdout.take().ok_or("Missing runtime stdout.")?;
        let (tx,lines) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || { for line in BufReader::new(stdout).lines() { if tx.send(line.map_err(|e|e.to_string())).is_err() { break; } } });
        let mut session = Self { child, stdin, lines, data: data.into(), owned: false };
        writeln!(session.stdin,"{}",serde_json::to_string(&init).map_err(|e|e.to_string())?).map_err(|e|e.to_string())?;
        let claimed = session.receive()?;
        if claimed["claimed"] != true { return Err("The Linux runtime could not claim its control session. See runtime.log.".into()); }
        session.owned = true;
        let ready = session.receive()?;
        if ready["ready"] != true { return Err("The Linux runtime did not initialize. See runtime.log.".into()); }
        Ok(session)
    }
    fn receive(&self) -> Result<Value,String> {
        let line = self.lines.recv_timeout(std::time::Duration::from_secs(300)).map_err(|_|"Linux runtime exited or timed out. See runtime.log.".to_string())??;
        serde_json::from_str(&line).map_err(|_|"Invalid runtime reply. See runtime.log.".into())
    }
    pub fn request(&mut self, command: &str, args: Value) -> Result<Value,String> {
        writeln!(self.stdin,"{}",json!({"command":command,"args":args})).and_then(|_|self.stdin.flush()).map_err(|e|e.to_string())?;
        let response = self.receive()?;
        if let Some(files) = response["files"].as_object() {
            for (name,body) in files {
                if !SYNC_FILES.contains(&name.as_str()) { return Err("Unexpected runtime file.".into()); }
                let path = self.data.join(name);
                std::fs::create_dir_all(path.parent().ok_or("Invalid sync path.")?).map_err(|e|e.to_string())?;
                // Replace only after the complete credentials/profile have been written.
                let temporary = path.with_extension("sync.tmp");
                std::fs::write(&temporary,body.as_str().ok_or("Invalid sync content.")?).and_then(|_|std::fs::rename(&temporary,&path)).map_err(|e|e.to_string())?;
                let _ = std::fs::remove_file(temporary);
            }
        }
        if let Some(error) = response["error"].as_str() { Err(error.into()) } else { Ok(response["result"].clone()) }
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        // EOF alone cannot stop a worker blocked inside Pi. Terminate this private distro and all its children.
        if self.owned { let _ = wsl().args(["--terminate","Wallis"]).status(); }
        let _ = self.child.kill(); let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(windows)]
    #[test]
    #[ignore = "requires the disposable Wallis distro and image from the CI workflow"]
    fn upgrade_preserves_data_and_failed_import_rolls_back() {
        assert_eq!(std::env::var("WALLIS_WSL_TEST").as_deref(), Ok("1"));
        let archive = PathBuf::from(std::env::var_os("WALLIS_WSL_ARCHIVE").expect("CI runtime image"));
        let data = std::env::temp_dir().join(format!("wallis-upgrade-test-{}", std::process::id()));
        std::fs::create_dir_all(&data).unwrap();
        run(&["-d","Wallis","-u","root","--","python3","-c","from pathlib import Path; Path('/var/lib/wallis/upgrade-probe').write_text('persistent'); Path('/var/lib/wallis/worker-scratch/upgrade-probe').write_text('workspace')"]).unwrap();
        let probe = || {
            run(&["-d","Wallis","-u","root","--","/opt/wallis/bin/wallis-bot","--self-check"]).unwrap();
            let output = wsl().args(["-d","Wallis","-u","root","--","cat","/var/lib/wallis/upgrade-probe","/var/lib/wallis/worker-scratch/upgrade-probe"]).output().unwrap();
            assert!(output.status.success());
            assert_eq!(output.stdout, b"persistentworkspace");
        };
        import_runtime(&data, &archive, true, &|_| {}).unwrap();
        probe();
        let backup = data.join("wallis-before-upgrade.tar");
        assert!(backup.is_file());
        assert!(import_runtime(&data, &archive, true, &|_| {}).unwrap_err().contains("earlier upgrade backup exists"));
        probe();
        // The successful upgrade is verified; make room for the next recovery export.
        std::fs::remove_file(backup).unwrap();
        let corrupt = data.join("corrupt.tar.gz");
        std::fs::write(&corrupt, "invalid runtime archive").unwrap();
        let error = import_runtime(&data, &corrupt, true, &|_| {}).unwrap_err();
        assert!(error.contains("original distro was restored"), "{error}");
        probe();
        run(&["--terminate","Wallis"]).unwrap();
        // Keep the imported distro's virtual disk until workflow cleanup unregisters it.
        for name in ["wallis-before-upgrade.tar", "wallis-user-data.tar", "corrupt.tar.gz"] { std::fs::remove_file(data.join(name)).unwrap(); }
    }

    #[test]
    fn wsl_output_handles_utf16_and_utf8() {
        let utf16: Vec<u8> = "Wallis\r\nOther".encode_utf16().flat_map(u16::to_le_bytes).collect();
        assert_eq!(decode(&utf16),"Wallis\r\nOther");
        assert_eq!(decode(b"Wallis\n"),"Wallis\n");
    }
}
