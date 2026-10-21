//! Pinned Linux runtime and speech models, assembled into the versioned WSL rootfs.
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub struct Artifact { pub name: &'static str, pub url: &'static str, pub sha256: &'static str, /// Leading folder to drop while unpacking.
    pub strip: &'static str }

pub const PI: Artifact = Artifact { name: "pi", url: "https://github.com/earendil-works/pi/releases/download/v1.2.0/pi-linux-x64.tar.gz", sha256: "00fc36cffdebceea5a48230da62cf3fb463178f8fe185766652f96002a246847", strip: "pi" };
pub const UV: Artifact = Artifact { name: "uv", url: "https://github.com/astral-sh/uv/releases/download/0.13.0/uv-x86_64-unknown-linux-gnu.tar.gz", sha256: "1468ebd5a5541121837c5a2817b9972ba6090fa6caa3d142620850a47fb75154", strip: "uv-x86_64-unknown-linux-gnu" };
/// Python packages installed into the app's own virtual environment (voice and realtime memory).
pub const SHERPA_PACKAGE: &str = "sherpa-onnx==1.13.8";
pub const TORCH_PACKAGE: &str = "torch==2.14.1+cpu";
pub const TORCHAUDIO_PACKAGE: &str = "torchaudio==2.11.0+cpu";
/// NeuTTS-2E's codec and watermarker. torchao is pinned because newer releases break torchtune, setuptools because Perth needs `pkg_resources`.
pub const NEUTTS_PACKAGES: [&str; 11] = ["local-attention==1.11.2", "torchtune==0.6.1", "transformers==5.20.2", "vector-quantize-pytorch==1.17.8", "torchao==0.11.0", "resemble-perth==1.0.1", "librosa==0.11.0", "soundfile==0.13.1", "setuptools==78.1.0", "requests==2.34.0", "einops==0.8.2"];
/// Installed without dependencies: only its four speakers' reference codes are used.
pub const NEUTTS_SAMPLES_PACKAGE: &str = "neutts==1.4.1";
/// Installed with `--no-deps` alongside the samples: it declares `torchao>=0.12`, which the pin above deliberately contradicts.
pub const NEUCODEC_PACKAGE: &str = "neucodec==0.0.6";
pub const MEMORY_PACKAGE: &str = "mnemosyne-memory[all]==3.15.1";
pub const MEDIA_PACKAGE: &str = "av==19.0.1";
pub const PYTHON_VERSION: &str = "3.12";

pub fn sha256_hex(bytes: &[u8]) -> String { Sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect() }

/// Downloads `url` to `dest`, keeping the file only if its SHA-256 matches `sha256`.
pub fn download_verified(url: &str, sha256: &str, dest: &Path) -> Result<(), String> { download_verified_as(url, sha256, dest, None) }

/// The same, sending `Authorization: Bearer <token>` to the first host (for gated Hugging Face files).
pub fn download_verified_as(url: &str, sha256: &str, dest: &Path, token: Option<&str>) -> Result<(), String> {
    if let Ok(mut existing) = std::fs::File::open(dest) {
        let mut hasher = Sha256::new(); let mut buffer = [0u8;65536];
        loop {
            let n = existing.read(&mut buffer).map_err(|e|e.to_string())?;
            if n == 0 { break; } hasher.update(&buffer[..n]);
        }
        if format!("{:x}",hasher.finalize()) == sha256 { return Ok(()); }
    }
    let parent = dest.parent().ok_or("Bad download path.")?;
    std::fs::create_dir_all(parent).map_err(|error| format!("Unable to create {}: {error}", parent.display()))?;
    let part = dest.with_extension("part");
    let mut request = ureq::get(url);
    if let Some(token) = token { request = request.header("Authorization", &format!("Bearer {token}")); }
    let mut response = request.call().map_err(|error| format!("Unable to download {url}: {}", if token.is_some() && error.to_string().contains("40") { "Hugging Face refused the token. Accept the NeuTTS-2E and NeuCodec terms with that account and use a read token.".to_string() } else { error.to_string() }))?;
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
    if artifact.url.ends_with(".tar.gz") {
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        run(Path::new("/usr/bin/tar"), &["-xzf", &archive.to_string_lossy(), "--strip-components=1", "-C", &dir.to_string_lossy()], "extract Linux runtime")?;
    } else { unzip(&archive, &dir, artifact.strip)?; }
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
    ("parakeet-0.6b-v3-int8", "Parakeet 0.6B v3 INT8 — multilingual, CPU", "5793d0fd397c5778d2cf2126994d58e9d56b1be7c04d13c7a15bb1b4eafb16bf", 487),
];
/// NeuTTS-2E's four fixed speakers and seven emotions (neuphonic/neutts-2e-q4-gguf).
pub const TTS_VOICES: [(&str, &str); 4] = [("emily", "Emily"), ("paul", "Paul"), ("sophie", "Sophie"), ("steven", "Steven")];
pub const EMOTIONS: [&str; 7] = ["angry", "disgusted", "fearful", "happy", "neutral", "sad", "surprised"];
const STT_ARCHIVE: &str = "sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8";
/// Gated on Hugging Face: CI downloads them with its own token under the NeuTTS Open License v1.0 and bakes them into the image.
pub const TTS_GGUF_URL: &str = "https://huggingface.co/neuphonic/neutts-2e-q4-gguf/resolve/021bd72d8eeff6f548e5c9209d03db6b6dff4939/neutts-2e-Q4_0.gguf";
pub const TTS_GGUF_SHA256: &str = "ccc2a33941aa3787d232c608e9a60f6cd74a2d2af0e1e35afce0aedccef38b1d";
pub const CODEC_URL: &str = "https://huggingface.co/neuphonic/neucodec/resolve/a940021324df13a07f44185f0a17c6dac3698179/pytorch_model.bin";
pub const CODEC_SHA256: &str = "30c3ea13ceeb2de693c56e5e33a1b7e00d44c95dcdd08a4ed0d552d0bf59ebdf";
/// Keeps only the decoder half of NeuCodec (the encoder is for cloning, which Wallis does not do) and the speakers' reference codes.
const PREPARE_TTS: &str = r#"
import importlib.util, json, sys, torch
from pathlib import Path
source, out = Path(sys.argv[1]), Path(sys.argv[2])
weights = torch.load(source, map_location="cpu", weights_only=True)
torch.save({k: v for k, v in weights.items() if k.startswith(("generator.", "fc_post_a."))}, out / "neucodec.bin")
samples = Path(importlib.util.find_spec("neutts").submodule_search_locations[0]) / "samples"
speakers = {n: {"codes": torch.load(samples / f"{n}.pt", weights_only=True).tolist(), "text": (samples / f"{n}.txt").read_text().strip()} for n in sys.argv[3:]}
(out / "speakers.json").write_text(json.dumps(speakers))
"#;
pub const SPEECH_SERVER: &str = include_str!("../py/speech_server.py");

/// Where everything lives under the app data folder.
pub struct Layout(pub PathBuf);

impl Layout {
    fn exe(name: &str) -> String { if cfg!(windows) { format!("{name}.exe") } else { name.to_string() } }
    pub fn tools(&self) -> PathBuf { self.0.join("tools") }
    pub fn pi(&self) -> PathBuf { self.tools().join("pi").join(Self::exe("pi")) }
    pub fn shell(&self) -> PathBuf { PathBuf::from("/bin/bash") }
    pub fn uv(&self) -> PathBuf { self.tools().join("uv").join(Self::exe("uv")) }
    pub fn venv(&self) -> PathBuf { self.tools().join("venv") }
    pub fn python(&self) -> PathBuf { if cfg!(windows) { self.venv().join("Scripts").join("python.exe") } else { self.venv().join("bin").join("python") } }
    pub fn speech_script(&self) -> PathBuf { self.tools().join("speech_server.py") }
    pub fn stt_model(&self, id: &str) -> PathBuf { self.0.join("models").join(id) }
    pub fn tts_model(&self) -> PathBuf { self.0.join("models/neutts-2e") }

}

fn run(program: &Path, args: &[&str], what: &str) -> Result<(), String> {
    let mut command = std::process::Command::new(program);
    command.args(args).stdin(std::process::Stdio::null());
    #[cfg(windows)]
    { use std::os::windows::process::CommandExt; command.creation_flags(0x0800_0000); }
    let output = command.output().map_err(|error| format!("Unable to {what}: {error}"))?;
    if output.status.success() { return Ok(()); }
    Err(format!("Unable to {what}: {}", String::from_utf8_lossy(&output.stderr).trim()))
}

/// Installs every runtime piece (idempotent): Pi, uv, a CPU Python environment with speech and memory packages, and pinned models.
/// `hf_token` is only needed for the gated NeuTTS-2E files, and not at all once they are on disk. `progress` receives a one-line description of each step.
pub fn ensure_all(layout: &Layout, stt: &str, hf_token: Option<&str>, progress: &dyn Fn(&str)) -> Result<(), String> {
    let stt_entry = STT_MODELS.iter().find(|m| m.0 == stt).ok_or("Unknown speech model.")?;
    let tools = layout.tools();
    progress("Installing the agent runtime…");
    install(&PI, &tools, &Layout::exe("pi"))?;
    progress("Installing speech recognition…");
    install(&UV, &tools, &Layout::exe("uv"))?;

    if !layout.python().is_file() {
        progress("Setting up Python for voice and memory…");
        run(&layout.uv(), &["venv", "--python", PYTHON_VERSION, &layout.venv().to_string_lossy()], "create the Python environment")?;
    }
    let packages = format!("{SHERPA_PACKAGE} {TORCH_PACKAGE} {TORCHAUDIO_PACKAGE} {} {NEUCODEC_PACKAGE} {NEUTTS_SAMPLES_PACKAGE} {MEMORY_PACKAGE} {MEDIA_PACKAGE}", NEUTTS_PACKAGES.join(" "));
    let marker = layout.venv().join("wallis-packages.txt");
    if std::fs::read_to_string(&marker).ok().as_deref() != Some(&packages) {
        progress("Installing CPU speech and memory packages…");
        let python = layout.python().to_string_lossy().into_owned();
        run(&layout.uv(), &["pip", "install", "--python", &python, "--index-url", "https://download.pytorch.org/whl/cpu", TORCH_PACKAGE, TORCHAUDIO_PACKAGE], "install CPU PyTorch")?;
        let mut install = vec!["pip", "install", "--python", &python, TORCH_PACKAGE, TORCHAUDIO_PACKAGE, SHERPA_PACKAGE, MEMORY_PACKAGE, MEDIA_PACKAGE];
        install.extend(NEUTTS_PACKAGES);
        run(&layout.uv(), &install, "install speech and memory packages")?;
        run(&layout.uv(), &["pip", "install", "--python", &python, "--no-deps", NEUCODEC_PACKAGE, NEUTTS_SAMPLES_PACKAGE], "install the NeuTTS codec and speaker samples")?;
        std::fs::write(marker, packages).map_err(|error| error.to_string())?;
    }
    std::fs::write(layout.speech_script(), SPEECH_SERVER).map_err(|error| format!("Unable to write the speech server: {error}"))?;
    progress("Downloading the local speech models…");
    let archive = tools.join("downloads").join(format!("{STT_ARCHIVE}.tar.bz2"));
    download_verified(&format!("https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/{STT_ARCHIVE}.tar.bz2"), stt_entry.2, &archive)?;
    let stt_dir = layout.stt_model(stt);
    if std::fs::read_to_string(stt_dir.join("installed.txt")).ok().as_deref() != Some(stt_entry.2) {
        run(&layout.python(), &["-c", "import sys,tarfile; tarfile.open(sys.argv[1]).extractall(sys.argv[2], filter='data')", &archive.to_string_lossy(), &stt_dir.to_string_lossy()], "extract Parakeet")?;
        for file in ["encoder.int8.onnx", "decoder.int8.onnx", "joiner.int8.onnx", "tokens.txt"] {
            if !stt_dir.join(STT_ARCHIVE).join(file).is_file() { return Err(format!("Parakeet installed without {file}.")); }
        }
        std::fs::write(stt_dir.join("installed.txt"), stt_entry.2).map_err(|error| error.to_string())?;
    }
    let tts = layout.tts_model();
    if !["neucodec.bin", "speakers.json"].iter().all(|f| tts.join(f).is_file()) {
        let token = hf_token.ok_or("Hugging Face token missing: NeuTTS-2E's gated files are downloaded with one (HF_TOKEN).")?;
        std::fs::create_dir_all(&tts).map_err(|error| format!("Unable to create {}: {error}", tts.display()))?;
        let full = tools.join("downloads/neucodec-full.bin");
        download_verified_as(CODEC_URL, CODEC_SHA256, &full, Some(token))?;
        run(&layout.python(), &["-c", PREPARE_TTS, &full.to_string_lossy(), &tts.to_string_lossy()].into_iter().chain(TTS_VOICES.iter().map(|v| v.0)).collect::<Vec<_>>(), "prepare the NeuTTS codec")?;
    }
    download_verified_as(TTS_GGUF_URL, TTS_GGUF_SHA256, &tts.join("neutts-2e-Q4_0.gguf"), hf_token)?;
    progress("");
    Ok(())
}

/// The two local speech servers. Dropping this stops them.
pub struct SpeechServers(Vec<std::process::Child>);

impl SpeechServers {
    pub fn start(layout: &Layout, stt: &str, threads: u32) -> Result<Self, String> {
        std::fs::create_dir_all(layout.0.join("logs")).map_err(|e| e.to_string())?;
        let spawn = |program: &Path, args: Vec<String>, what: &str, port: u16| {
            let mut command = std::process::Command::new(program);
            let log = std::fs::File::create(layout.0.join("logs").join(format!("speech-{port}.log"))).map_err(|error| error.to_string())?;
            command.args(args).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(log);
            #[cfg(windows)]
            { use std::os::windows::process::CommandExt; command.creation_flags(0x0800_0000); }
            command.spawn().map_err(|error| format!("Unable to start {what}: {error}"))
        };
        if !STT_MODELS.iter().any(|m| m.0 == stt) || !(1..=64).contains(&threads) { return Err("Invalid speech settings.".into()); }
        let stt_server = spawn(&layout.python(), vec!["-u".into(), layout.speech_script().to_string_lossy().into(), "stt".into(), layout.stt_model(stt).join(STT_ARCHIVE).to_string_lossy().into(), threads.to_string()], "speech recognition", 8085)?;
        let mut servers = Self(vec![stt_server]);
        servers.0.push(spawn(&layout.python(), vec!["-u".into(), layout.speech_script().to_string_lossy().into(), "tts".into(), layout.tts_model().to_string_lossy().into(), threads.to_string()], "the voice", 8086)?);
        for (name, port) in [("speech recognition", 8085u16), ("the voice", 8086)] {
            let mut up = false;
            for _ in 0..900 {
                for (index, child) in servers.0.iter_mut().enumerate() {
                    if child.try_wait().map_err(|error| error.to_string())?.is_some() {
                        let log = std::fs::read_to_string(layout.0.join("logs").join(format!("speech-{}.log", 8085 + index))).unwrap_or_default();
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
        assert!(layout.stt_model("parakeet-0.6b-v3-int8").ends_with("models/parakeet-0.6b-v3-int8") && layout.tts_model().ends_with("models/neutts-2e"));
        assert!(layout.python().starts_with("data/tools/venv"));
        assert!(STT_MODELS.iter().map(|m| m.2).chain([TTS_GGUF_SHA256, CODEC_SHA256]).all(|sum| sum.len() == 64));
        assert!(SPEECH_SERVER.contains("PerthImplicitWatermarker") && SPEECH_SERVER.contains(&format!("SPEAKERS = {:?}", TTS_VOICES.map(|v| v.0)).replace('[', "(").replace(']', ")")));
    }
}
