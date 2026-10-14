//! Windows smoke test: `smoke <data_dir>`. Installs Pi + the worker shell, then proves the sandbox confines a process.
#[cfg(windows)]
fn main() -> Result<(), String> {
    use std::io::Read;
    use wallis_core::agent::Access;
    use wallis_core::provision::{self, Layout};
    let data = std::path::PathBuf::from(std::env::args().nth(1).ok_or("usage: smoke <data_dir>")?);
    let layout = Layout(data.clone());
    println!("install pi");
    provision::install(&provision::PI, &layout.tools(), "pi.exe")?;
    println!("install shell");
    provision::download_verified(provision::BUSYBOX_URL, provision::BUSYBOX_SHA256, &layout.shell())?;
    let shell = layout.shell();
    let scratch = data.join("scratch");
    std::fs::create_dir_all(&scratch).map_err(|e| e.to_string())?;
    let secret = data.join("outside.txt");
    std::fs::write(&secret, "secret").map_err(|e| e.to_string())?;
    let access = Access { read: vec![layout.tools()], write: vec![scratch.clone()], path: vec![] };
    let script = format!("exec 2>{s}/err.txt; echo inside > {s}/ok.txt && echo WRITE_SCRATCH_OK; cat '{o}' && echo READ_OUTSIDE_LEAKED; echo x > '{o}.new' && echo WRITE_OUTSIDE_LEAKED; echo DONE", s = scratch.to_string_lossy().replace('\\', "/"), o = secret.to_string_lossy().replace('\\', "/"));
    let env = vec![("SystemRoot".to_string(), std::env::var("SystemRoot").unwrap_or_default()), ("PATH".to_string(), format!(r"{}\System32", std::env::var("SystemRoot").unwrap_or_default()))];
    let mut child = Some(wallis_core::sandbox::spawn(&shell, &["-c".to_string(), script.clone()], &scratch, &env, &access)?);
    let mut child = child.ok_or("no variant started")?;
    let sysroot = std::env::var("SystemRoot").unwrap_or_default();
    let copy = layout.tools().join("cmd-copy.exe");
    std::fs::copy(format!(r"{sysroot}\System32\cmd.exe"), &copy).map_err(|e| e.to_string())?;
    let mut basic = env.clone();
    for key in ["LOCALAPPDATA", "APPDATA", "ProgramData", "windir", "ComSpec", "USERPROFILE", "TEMP", "TMP"] { if let Ok(v) = std::env::var(key) { basic.push((key.to_string(), v)); } }
    let programs: Vec<(String, std::path::PathBuf, Vec<String>)> = vec![
        ("system cmd".into(), format!(r"{sysroot}\System32\cmd.exe").into(), vec!["/c".into(), "echo CMD_OK".into()]),
        ("whoami".into(), format!(r"{sysroot}\System32\whoami.exe").into(), vec![]),
        ("tools cmd copy".into(), copy, vec!["/c".into(), "echo CMD_OK".into()]),
        ("sh".into(), shell.clone(), vec!["-c".into(), "echo SH_OK".into()]),
        ("pi".into(), layout.pi(), vec!["--version".into()]),
    ];
    for (label, environment) in [("min", &env), ("basic", &basic)] {
        for (name, program, args) in &programs {
            let mut probe = wallis_core::sandbox::spawn(program, args, &scratch, environment, &access)?;
            std::thread::sleep(std::time::Duration::from_secs(2));
            let mut text = String::new();
            let _ = std::io::Read::read_to_string(&mut probe.stdout, &mut text);
            println!("[{label}] {name}: exit {:?} output {:?}", probe.exit_code().map(|c| format!("{c:#x}")), text.trim());
        }
    }
    let mut out = String::new();
    child.stdout.read_to_string(&mut out).map_err(|e| e.to_string())?;
    println!("--- sandbox output\n{out}---");
    println!("sh exit code: {:?}", child.exit_code().map(|c| format!("{c:#x}")));
    println!("sh stderr: {}", std::fs::read_to_string(scratch.join("err.txt")).unwrap_or_default());
    let wrote = scratch.join("ok.txt").is_file();
    println!("scratch write: {}", if out.contains("WRITE_SCRATCH_OK") && wrote { "PASS" } else { "FAIL" });
    println!("outside read blocked: {}", if out.contains("READ_OUTSIDE_LEAKED") { "FAIL" } else { "PASS" });
    println!("outside write blocked: {}", if out.contains("WRITE_OUTSIDE_LEAKED") || data.join("outside.txt.new").exists() { "FAIL" } else { "PASS" });
    let mut pi = wallis_core::sandbox::spawn(&layout.pi(), &["--version".to_string()], &scratch, &env, &access)?;
    let mut version = String::new();
    pi.stdout.read_to_string(&mut version).map_err(|e| e.to_string())?;
    println!("pi exit code: {:?}", pi.exit_code().map(|c| format!("{c:#x}")));
    println!("sandboxed pi --version: {}", version.trim());
    Ok(())
}
#[cfg(not(windows))]
fn main() {}
