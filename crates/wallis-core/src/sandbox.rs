//! Linux worker isolation: separate uid, read-only filesystem, private processes and bounded lifetime.
use crate::agent::Access;
use std::path::Path;
use std::process::{Child, Command};
#[cfg(target_os = "linux")]
use std::process::Stdio;

pub fn command(program: &Path, args: &[String], cwd: &Path, env: &[(String, String)], access: &Access, mods_only: bool) -> Result<Command, String> {
    #[cfg(not(target_os = "linux"))]
    { let _ = (program, args, cwd, env, access, mods_only); Err("Workers and mods require the Wallis Linux environment.".into()) }
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt;
        let mut command = Command::new("/usr/bin/sudo");
        command.args(["-n", "-u", "worker", "--", "/usr/bin/timeout", "--signal=KILL", "300", "/usr/bin/prlimit", "--fsize=52428800", "--nproc=256", "--", "/usr/bin/bwrap", "--die-with-parent", "--unshare-pid", "--unshare-ipc", "--unshare-uts", "--ro-bind", "/", "/", "--proc", "/proc", "--dev", "/dev", "--tmpfs", "/tmp", "--tmpfs", "/run", "--tmpfs", "/mnt", "--tmpfs", "/var/lib/wallis", "--clearenv", "--setenv", "HOME", &cwd.to_string_lossy(), "--setenv", "PATH", "/opt/wallis/tools/bun:/opt/wallis/tools/pi:/usr/local/bin:/opt/wallis/tools/node/node_modules/.bin:/opt/wallis/tools/venv/bin:/usr/bin:/bin"]);
        command.args(["--setenv","AGENT_BROWSER_EXECUTABLE_PATH","/opt/wallis/tools/clearcote/chrome","--setenv","AGENT_BROWSER_ARGS","--no-sandbox,--disable-dev-shm-usage"]);
        // WSL links resolv.conf into /mnt/wsl, which is hidden above.
        let resolver = std::fs::canonicalize("/etc/resolv.conf").map_err(|e| format!("Unable to resolve DNS configuration: {e}"))?;
        command.arg("--ro-bind").arg(&resolver).arg(&resolver);
        for path in &access.read { command.arg("--ro-bind").arg(path).arg(path); }
        for path in &access.write { command.arg("--bind").arg(path).arg(path); }
        // Mods cannot read the worker's provider credentials either.
        let _ = mods_only; // The writable grants determine whether the worker config is visible.
        for (key, value) in env { command.args(["--setenv", key, value]); }
        command.arg("--chdir").arg(cwd).arg("--").arg(program).args(args);
        command.process_group(0);
        Ok(command)
    }
}

pub fn kill(child: &mut Child, confined: bool) {
    #[cfg(not(target_os = "linux"))]
    let _ = confined;
    #[cfg(target_os = "linux")]
    if confined {
        // sudo owns a different uid; ask that uid to kill the entire process group, including browser daemons.
        let _ = Command::new("/usr/bin/sudo").args(["-n", "-u", "worker", "--", "/usr/bin/kill", "-KILL", "--", &format!("-{}", child.id())]).stdout(Stdio::null()).stderr(Stdio::null()).status();
    }
    #[cfg(target_os = "linux")]
    if !confined { let _ = Command::new("/bin/kill").args(["-KILL","--",&format!("-{}",child.id())]).stdout(Stdio::null()).stderr(Stdio::null()).status(); }
    let _ = child.kill();
    let _ = child.wait();
}
