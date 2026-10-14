//! Windows check: `provision <data_dir>` runs the full install, starts both speech servers, and round-trips Pocket TTS -> Parakeet.
fn main() -> Result<(), String> {
    use wallis_core::provision::{ensure_all, Layout, SpeechServers};
    let layout = Layout(std::path::PathBuf::from(std::env::args().nth(1).ok_or("usage: provision <data_dir>")?));
    let (stt, tts) = ("parakeet-110m-int8", "alba");
    if !std::env::args().any(|arg| arg == "--installed") { ensure_all(&layout, stt, tts, &|step| if !step.is_empty() { println!("{step}") })?; }
    let _servers = SpeechServers::start(&layout, stt, 4, tts)?;
    println!("speech servers up");
    let pcm = wallis_core::speech::synthesize("Hello, this is a test of the Wallis voice.")?;
    println!("synthesized {} samples", pcm.len());
    let text = wallis_core::speech::transcribe(&pcm)?;
    println!("transcribed: {text:?}");
    if !text.to_lowercase().contains("test") { return Err("Speech roundtrip did not recognize the test sentence.".into()); }
    Ok(())
}
