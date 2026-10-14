//! Windows check: `provision <data_dir>` runs the full install, starts both speech servers, and round-trips Piper -> whisper.
#[cfg(windows)]
fn main() -> Result<(), String> {
    use wallis_core::provision::{ensure_all, Layout, SpeechServers};
    let layout = Layout(std::path::PathBuf::from(std::env::args().nth(1).ok_or("usage: provision <data_dir>")?));
    let (stt, tts) = ("base.en", "medium");
    ensure_all(&layout, stt, tts, &|step| if !step.is_empty() { println!("{step}") })?;
    let _servers = SpeechServers::start(&layout, stt, 4, tts)?;
    println!("speech servers up");
    let pcm = wallis_core::speech::synthesize("Hello, this is a test of the Wallis voice.", 1.0)?;
    println!("synthesized {} samples", pcm.len());
    println!("transcribed: {:?}", wallis_core::speech::transcribe(&pcm)?);
    Ok(())
}
#[cfg(not(windows))]
fn main() {}
