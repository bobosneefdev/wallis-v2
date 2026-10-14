//! Native Windows install: pinned, checksummed downloads unpacked under the app data folder. No WSL, no admin rights.
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub struct Artifact { pub name: &'static str, pub url: &'static str, pub sha256: &'static str, /// Leading folder to drop while unpacking.
    pub strip: &'static str }

pub const PI: Artifact = Artifact { name: "pi", url: "https://github.com/earendil-works/pi/releases/download/v1.2.0/pi-windows-x64.zip", sha256: "bd272b508657ad8d0ac6176e7751913c848a89cfb8cbe21ff319941e6ca140be", strip: "" };
pub const WHISPER: Artifact = Artifact { name: "whisper", url: "https://github.com/ggml-org/whisper.cpp/releases/download/b4938/whisper-bin-x64.zip", sha256: "c2a4b60edb11f7e11a9191ffb50929535527d4d91c9903dbe3e554583bbbc63d", strip: "Release/" };
pub const UV: Artifact = Artifact { name: "uv", url: "https://github.com/astral-sh/uv/releases/download/0.13.0/uv-x86_64-pc-windows-msvc.zip", sha256: "088962f9e7b7bd9ea740c04c650b2a21c8928c345bd99ac24350dc924dba656c", strip: "" };
/// Python packages installed into the app's own virtual environment (voice and realtime memory).
pub const PIPER_PACKAGE: &str = "piper-tts==1.8.0";
pub const MEMORY_PACKAGE: &str = "mnemosyne-memory[all]==3.15.1";
pub const PYTHON_VERSION: &str = "3.12";

pub fn sha256_hex(bytes: &[u8]) -> String { Sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect() }

/// Downloads `url` to `dest`, keeping the file only if its SHA-256 matches `sha256`.
pub fn download_verified(url: &str, sha256: &str, dest: &Path) -> Result<(), String> {
    if let Ok(existing) = std::fs::read(dest) { if sha256_hex(&existing) == sha256 { return Ok(()); } }
    let parent = dest.parent().ok_or("Bad download path.")?;
    std::fs::create_dir_all(parent).map_err(|error| format!("Unable to create {}: {error}", parent.display()))?;
    let part = dest.with_extension("part");
    let mut response = ureq::get(url).call().map_err(|error| format!("Unable to download {url}: {error}"))?;
    let (mut file, mut hasher, mut buffer) = (std::fs::File::create(&part).map_err(|error| error.to_string())?, Sha256::new(), [0u8; 65536]);
    let mut reader = response.body_mut().as_reader();
    loop {
        let read = reader.read(&mut buffer).map_err(|error| format!("Download of {url} was interrupted: {error}"))?;
        if read == 0 { break; }
        hasher.update(&buffer[..read]);
        file.write_all(&buffer[..read]).map_err(|error| error.to_string())?;
    }
    let actual: String = hasher.finalize().iter().map(|byte| format!("{byte:02x}")).collect();
    if actual != sha256 { let _ = std::fs::remove_file(&part); return Err(format!("Checksum mismatch for {url}: expected {sha256}, got {actual}.")); }
    std::fs::rename(&part, dest).map_err(|error| error.to_string())
}

/// Unpacks `archive` into `dest`, dropping `strip` and refusing any entry that would land outside `dest`.
pub fn unzip(archive: &Path, dest: &Path, strip: &str) -> Result<(), String> {
    let mut zip = zip::ZipArchive::new(std::fs::File::open(archive).map_err(|error| error.to_string())?).map_err(|error| format!("Unreadable archive: {error}"))?;
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index).map_err(|error| error.to_string())?;
        let relative = entry.enclosed_name().ok_or_else(|| format!("Unsafe path in archive: {}", entry.name()))?;
        let Ok(relative) = relative.strip_prefix(strip.trim_end_matches('/')) else { continue };
        if relative.as_os_str().is_empty() { continue; }
        let target = dest.join(relative);
        if entry.is_dir() { std::fs::create_dir_all(&target).map_err(|error| error.to_string())?; continue; }
        if let Some(parent) = target.parent() { std::fs::create_dir_all(parent).map_err(|error| error.to_string())?; }
        std::io::copy(&mut entry, &mut std::fs::File::create(&target).map_err(|error| format!("Unable to write {}: {error}", target.display()))?).map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// Installs `artifact` into `<root>/<name>` unless its marker file is already there.
pub fn install(artifact: &Artifact, root: &Path, marker: &str) -> Result<PathBuf, String> {
    let dir = root.join(artifact.name);
    if dir.join(marker).is_file() { return Ok(dir); }
    let archive = root.join("downloads").join(format!("{}.zip", artifact.name));
    download_verified(artifact.url, artifact.sha256, &archive)?;
    let _ = std::fs::remove_dir_all(&dir);
    unzip(&archive, &dir, artifact.strip)?;
    if !dir.join(marker).is_file() { return Err(format!("{} installed without {marker}.", artifact.name)); }
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use zip::write::SimpleFileOptions;

    fn make_zip(path: &Path, files: &[(&str, &str)]) {
        let mut writer = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        for (name, body) in files { writer.start_file(*name, SimpleFileOptions::default()).unwrap(); writer.write_all(body.as_bytes()).unwrap(); }
        writer.finish().unwrap();
    }

    #[test]
    fn unzips_with_prefix_stripped_and_skips_files_outside_it() {
        let dir = std::env::temp_dir().join(format!("wallis-prov-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        make_zip(&dir.join("a.zip"), &[("Release/whisper-server.exe", "exe"), ("Release/sub/x.dll", "dll"), ("other/readme", "no")]);
        unzip(&dir.join("a.zip"), &dir.join("out"), "Release/").unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("out/whisper-server.exe")).unwrap(), "exe");
        assert!(dir.join("out/sub/x.dll").is_file() && !dir.join("out/readme").exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn refuses_archives_that_escape_the_install_folder() {
        let dir = std::env::temp_dir().join(format!("wallis-prov-evil-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        make_zip(&dir.join("evil.zip"), &[("../escaped.txt", "x")]);
        assert!(unzip(&dir.join("evil.zip"), &dir.join("out"), "").unwrap_err().contains("Unsafe path"));
        assert!(!dir.join("escaped.txt").exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_existing_download_with_the_right_checksum_is_reused_without_network() {
        let dir = std::env::temp_dir().join(format!("wallis-prov-cache-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("f"), b"hello").unwrap();
        let sum = sha256_hex(b"hello");
        assert_eq!(sum, "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824");
        download_verified("http://127.0.0.1:9/never", &sum, &dir.join("f")).unwrap();
        assert!(download_verified("http://127.0.0.1:9/never", "bad", &dir.join("f")).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn pinned_checksums_are_well_formed() {
        for artifact in [&PI, &WHISPER, &UV] { assert!(artifact.sha256.len() == 64 && artifact.sha256.bytes().all(|b| b.is_ascii_hexdigit()), "{}", artifact.name); }
    }
}
