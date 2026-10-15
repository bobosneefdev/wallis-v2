//! Native Windows install: pinned, checksummed downloads unpacked under the app data folder. No WSL, no admin rights.
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub struct Artifact { pub name: &'static str, pub url: &'static str, pub sha256: &'static str, /// Leading folder to drop while unpacking.
    pub strip: &'static str }

pub const PI: Artifact = Artifact { name: "pi", url: "https://github.com/earendil-works/pi/releases/download/v1.2.0/pi-windows-x64.zip", sha256: "bd272b508657ad8d0ac6176e7751913c848a89cfb8cbe21ff319941e6ca140be", strip: "" };
pub const UV: Artifact = Artifact { name: "uv", url: "https://github.com/astral-sh/uv/releases/download/0.13.0/uv-x86_64-pc-windows-msvc.zip", sha256: "088962f9e7b7bd9ea740c04c650b2a21c8928c345bd99ac24350dc924dba656c", strip: "" };
/// busybox-w32 (single exe, applets built in): the worker's `sh`. MSYS shells cannot initialise inside an AppContainer.
pub const BUSYBOX_URL: &str = "https://frippery.org/files/busybox/busybox-w64u-FRP-6075-g169694ebd.exe";
pub const BUSYBOX_SHA256: &str = "6e263d154d8548d1eb936f65d1d8312c80df31c45974e48d6335e4dcc0f4f34c";
/// Python packages installed into the app's own virtual environment (voice and realtime memory).
pub const POCKET_PACKAGE: &str = "pocket-tts==3.3.0";
pub const SHERPA_PACKAGE: &str = "sherpa-onnx==1.13.8";
pub const TORCH_PACKAGE: &str = "torch==2.14.1+cpu";
pub const MEMORY_PACKAGE: &str = "mnemosyne-memory[all]==3.15.1";
pub const MEDIA_PACKAGE: &str = "av==19.0.1";
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
        for artifact in [&PI, &UV] { assert!(artifact.sha256.len() == 64 && artifact.sha256.bytes().all(|b| b.is_ascii_hexdigit()), "{}", artifact.name); }
    }
}

/// Speech models the GUI can choose between: (id, label, sha256, megabytes).
pub const STT_MODELS: [(&str, &str, &str, u32); 1] = [
    ("parakeet-110m-int8", "Parakeet 110M INT8 — English, CPU", "f628312e9fdf8686374cb01a69425c41732529d540860311f16f37cbc32cfe9b", 108),
];
/// Permanent Pocket preset states; no voice cloning or Hugging Face account required.
pub const TTS_VOICES: [(&str, &str, &str, u32); 3] = [
    ("alba", "Alba", "d291428b416d6c36a1de7835e51dbe1e334b75e5af512bb18dd23a1047fe8f3b", 6),
    ("marius", "Marius", "b05946f39371f8853902b0486231f2d401c8a9570da9978329120d4ecfd009e9", 6),
    ("javert", "Javert", "580e4b72d4065e906c14e580f97da478c9707bead74b0ef9c049e0ca8de997d9", 6),
];
const STT_ARCHIVE: &str = "sherpa-onnx-nemo-parakeet_tdt_transducer_110m-en-36000-int8";
const POCKET_BASE: &str = "https://huggingface.co/kyutai/pocket-tts-without-voice-cloning/resolve/4e1e0a3e611c51c0b4ed8174fc10f32a54644303/languages/english_2026-09";
pub const SPEECH_SERVER: &str = include_str!("../py/speech_server.py");

/// Where everything lives under the app data folder.
pub struct Layout(pub PathBuf);

impl Layout {
    fn exe(name: &str) -> String { if cfg!(windows) { format!("{name}.exe") } else { name.to_string() } }
    pub fn tools(&self) -> PathBuf { self.0.join("tools") }
    pub fn pi(&self) -> PathBuf { self.tools().join("pi").join(Self::exe("pi")) }
    pub fn shell(&self) -> PathBuf { self.tools().join("busybox").join(Self::exe("sh")) }
    pub fn uv(&self) -> PathBuf { self.tools().join("uv").join(Self::exe("uv")) }
    pub fn venv(&self) -> PathBuf { self.tools().join("venv") }
    pub fn python(&self) -> PathBuf { if cfg!(windows) { self.venv().join("Scripts").join("python.exe") } else { self.venv().join("bin").join("python") } }
    pub fn speech_script(&self) -> PathBuf { self.tools().join("speech_server.py") }
    pub fn stt_model(&self, id: &str) -> PathBuf { self.0.join("models").join(id) }
    pub fn tts_model(&self, id: &str) -> PathBuf { self.0.join("models/pocket").join(format!("{id}.safetensors")) }
}

fn run(program: &Path, args: &[&str], what: &str) -> Result<(), String> {
    let mut command = std::process::Command::new(program);
    command.args(args).stdin(std::process::Stdio::null());
    #[cfg(windows)]
    { use std::os::windows::process::CommandExt; command.creation_flags(0x0800_0000); }
    let output = command.output().map_err(|error| format!("Unable to {what}: {error}"))?;
    if output.status.success() { return Ok(()); }
    Err(format!("Unable to {what}: {}", String::from_utf8_lossy(&output.stderr).lines().rev().find(|line| !line.trim().is_empty()).unwrap_or("no output")))
}

/// Installs every runtime piece (idempotent): Pi, uv, a CPU Python environment with speech and memory packages, and pinned models.
/// `progress` receives a one-line description of each step.
pub fn ensure_all(layout: &Layout, stt: &str, tts: &str, progress: &dyn Fn(&str)) -> Result<(), String> {
    let (stt_entry, tts_entry) = (STT_MODELS.iter().find(|m| m.0 == stt).ok_or("Unknown speech model.")?, TTS_VOICES.iter().find(|m| m.0 == tts).ok_or("Unknown voice.")?);
    let tools = layout.tools();
    progress("Installing the agent runtime…");
    install(&PI, &tools, &Layout::exe("pi"))?;
    progress("Installing speech recognition…");
    install(&UV, &tools, &Layout::exe("uv"))?;
    progress("Installing the worker shell…");
    download_verified(BUSYBOX_URL, BUSYBOX_SHA256, &layout.shell())?;
    if !layout.python().is_file() {
        progress("Setting up Python for voice and memory…");
        run(&layout.uv(), &["venv", "--python", PYTHON_VERSION, &layout.venv().to_string_lossy()], "create the Python environment")?;
    }
    let packages = format!("{POCKET_PACKAGE} {SHERPA_PACKAGE} {TORCH_PACKAGE} {MEMORY_PACKAGE} {MEDIA_PACKAGE}");
    let marker = layout.venv().join("wallis-packages.txt");
    if std::fs::read_to_string(&marker).ok().as_deref() != Some(&packages) {
        progress("Installing CPU speech and memory packages…");
        run(&layout.uv(), &["pip", "install", "--python", &layout.python().to_string_lossy(), "--index-url", "https://download.pytorch.org/whl/cpu", TORCH_PACKAGE], "install CPU PyTorch")?;
        run(&layout.uv(), &["pip", "install", "--python", &layout.python().to_string_lossy(), TORCH_PACKAGE, POCKET_PACKAGE, SHERPA_PACKAGE, MEMORY_PACKAGE, MEDIA_PACKAGE], "install speech and memory packages")?;
        std::fs::write(marker, packages).map_err(|error| error.to_string())?;
    }
    std::fs::write(layout.speech_script(), SPEECH_SERVER).map_err(|error| format!("Unable to write the speech server: {error}"))?;
    progress("Downloading the local speech models…");
    let archive = tools.join("downloads/parakeet.tar.bz2");
    download_verified(&format!("https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/{STT_ARCHIVE}.tar.bz2"), stt_entry.2, &archive)?;
    let stt_dir = layout.stt_model(stt);
    if std::fs::read_to_string(stt_dir.join("installed.txt")).ok().as_deref() != Some(stt_entry.2) {
        run(&layout.python(), &["-c", "import sys,tarfile; tarfile.open(sys.argv[1]).extractall(sys.argv[2], filter='data')", &archive.to_string_lossy(), &stt_dir.to_string_lossy()], "extract Parakeet")?;
        for file in ["encoder.int8.onnx", "decoder.int8.onnx", "joiner.int8.onnx", "tokens.txt"] {
            if !stt_dir.join(STT_ARCHIVE).join(file).is_file() { return Err(format!("Parakeet installed without {file}.")); }
        }
        std::fs::write(stt_dir.join("installed.txt"), stt_entry.2).map_err(|error| error.to_string())?;
    }
    let pocket = layout.tts_model(tts).parent().unwrap().to_path_buf();
    download_verified(&format!("{POCKET_BASE}/model.safetensors"), "916ccd2686e9311cb40054893a3c4284393d658825ffc714a276f3e9b152344f", &pocket.join("model.safetensors"))?;
    download_verified(&format!("{POCKET_BASE}/tokenizer.json"), "f498428e1eafee50492f7be13dc9bfafcfc12e508cd0eb1b01c92ecd5d8c6687", &pocket.join("tokenizer.json"))?;
    download_verified(&format!("{POCKET_BASE}/embeddings/{tts}.safetensors"), tts_entry.2, &layout.tts_model(tts))?;
    progress("");
    Ok(())
}

/// The two local speech servers. Dropping this stops them.
pub struct SpeechServers(Vec<std::process::Child>);

impl SpeechServers {
    pub fn start(layout: &Layout, stt: &str, threads: u32, tts: &str) -> Result<Self, String> {
        let spawn = |program: &Path, args: Vec<String>, what: &str, port: u16| {
            let mut command = std::process::Command::new(program);
            let log = std::fs::File::create(layout.tools().join(format!("speech-{port}.log"))).map_err(|error| error.to_string())?;
            command.args(args).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(log);
            #[cfg(windows)]
            { use std::os::windows::process::CommandExt; command.creation_flags(0x0800_0000); }
            command.spawn().map_err(|error| format!("Unable to start {what}: {error}"))
        };
        if !STT_MODELS.iter().any(|m| m.0 == stt) || !TTS_VOICES.iter().any(|m| m.0 == tts) || !(1..=64).contains(&threads) { return Err("Invalid speech settings.".into()); }
        let stt_server = spawn(&layout.python(), vec!["-u".into(), layout.speech_script().to_string_lossy().into(), "stt".into(), layout.stt_model(stt).join(STT_ARCHIVE).to_string_lossy().into(), threads.to_string()], "speech recognition", 8085)?;
        let mut servers = Self(vec![stt_server]);
        servers.0.push(spawn(&layout.python(), vec!["-u".into(), layout.speech_script().to_string_lossy().into(), "tts".into(), layout.tts_model(tts).to_string_lossy().into(), threads.to_string()], "the voice", 8086)?);
        for (name, port) in [("speech recognition", 8085u16), ("the voice", 8086)] {
            let mut up = false;
            for _ in 0..300 {
                for (index, child) in servers.0.iter_mut().enumerate() {
                    if child.try_wait().map_err(|error| error.to_string())?.is_some() {
                        let log = std::fs::read_to_string(layout.tools().join(format!("speech-{}.log", 8085 + index))).unwrap_or_default();
                        return Err(format!("Speech server exited: {}", log.lines().last().unwrap_or("no output")));
                    }
                }
                if ureq::get(format!("http://127.0.0.1:{port}/health")).config().timeout_global(Some(std::time::Duration::from_secs(1))).build().call().is_ok() { up = true; break; }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            if !up { return Err(format!("{name} did not start. See tools/speech-{port}.log.")); }
        }
        Ok(servers)
    }
}

impl Drop for SpeechServers {
    fn drop(&mut self) { for child in &mut self.0 { let _ = child.kill(); let _ = child.wait(); } }
}

#[cfg(test)]
mod layout_tests {
    use super::*;

    #[test]
    fn layout_keeps_models_and_tools_apart_and_voices_are_pinned() {
        let layout = Layout(PathBuf::from("data"));
        assert!(layout.stt_model("parakeet-110m-int8").ends_with("models/parakeet-110m-int8") && layout.tts_model("alba").ends_with("models/pocket/alba.safetensors"));
        assert!(layout.python().starts_with("data/tools/venv"));
        assert!(STT_MODELS.iter().chain(TTS_VOICES.iter()).all(|m| m.2.len() == 64));
        assert!(SPEECH_SERVER.contains("TTSModel.load_model"));
    }
}
