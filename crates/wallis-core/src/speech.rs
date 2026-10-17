//! Speech plumbing between Discord (48 kHz stereo PCM), Parakeet (16 kHz mono WAV) and Pocket TTS (24 kHz mono f32 PCM).
use std::io::Read;

pub const STT_URL: &str = "http://127.0.0.1:8085";
pub const TTS_URL: &str = "http://127.0.0.1:8086";
const TTS_RATE: usize = 24_000;

/// Discord voice -> Parakeet: average each 3 stereo frames (48 kHz -> 16 kHz) into one mono sample.
pub fn downsample(pcm: &[i16]) -> Vec<i16> {
    pcm.chunks_exact(6).map(|frame| (frame.iter().map(|&s| s as i32).sum::<i32>() / 6) as i16).collect()
}

/// 16 kHz mono PCM as a WAV file.
pub fn wav16k(samples: &[i16]) -> Vec<u8> {
    let data = (samples.len() * 2) as u32;
    let mut wav = Vec::with_capacity(44 + samples.len() * 2);
    wav.extend(b"RIFF"); wav.extend((36 + data).to_le_bytes()); wav.extend(b"WAVEfmt ");
    wav.extend(16u32.to_le_bytes()); wav.extend(1u16.to_le_bytes()); wav.extend(1u16.to_le_bytes());
    wav.extend(16_000u32.to_le_bytes()); wav.extend(32_000u32.to_le_bytes()); wav.extend(2u16.to_le_bytes()); wav.extend(16u16.to_le_bytes());
    wav.extend(b"data"); wav.extend(data.to_le_bytes());
    wav.extend(samples.iter().flat_map(|sample| sample.to_le_bytes()));
    wav
}

/// Pocket TTS output -> Discord voice: linear-interpolate 24 kHz mono up to 48 kHz and duplicate to stereo.
pub fn pocket_to_discord(mono: &[i16]) -> Vec<i16> {
    if mono.is_empty() { return vec![]; }
    let frames = mono.len() * 48_000 / TTS_RATE;
    let mut out = Vec::with_capacity(frames * 2);
    for frame in 0..frames {
        let position = frame as f64 * TTS_RATE as f64 / 48_000.0;
        let (index, fraction) = (position as usize, position.fract());
        let (a, b) = (mono[index] as f64, mono[(index + 1).min(mono.len() - 1)] as f64);
        let sample = (a + (b - a) * fraction) as i16;
        out.extend([sample, sample]);
    }
    out
}

/// Whisper marks non-speech as bracketed events such as `[BLANK_AUDIO]` or `(music)`; drop them.
pub fn clean_transcript(text: &str) -> String {
    let mut out = String::new();
    let mut depth = 0u32;
    for ch in text.chars() {
        match ch { '[' | '(' => depth += 1, ']' | ')' => depth = depth.saturating_sub(1), _ if depth == 0 => out.push(ch), _ => {} }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Speech to text through the local Parakeet server.
pub fn transcribe(pcm_48k_stereo: &[i16]) -> Result<String, String> {
    let response: serde_json::Value = ureq::post(format!("{STT_URL}/inference")).config().timeout_global(Some(std::time::Duration::from_secs(60))).build()
        .header("Content-Type", "audio/wav")
        .send(&wav16k(&downsample(pcm_48k_stereo))).map_err(|error| format!("Parakeet failed: {error}"))?
        .body_mut().read_json().map_err(|error| format!("Parakeet sent unreadable output: {error}"))?;
    Ok(clean_transcript(response["text"].as_str().ok_or("Parakeet returned no transcript field.")?))
}

/// Streaming 24 kHz mono little-endian f32 PCM. Dropping the reader cancels the HTTP stream.
pub fn synthesize_stream(text: &str, tuning: &crate::tuning::Tuning) -> Result<impl Read + Send + Sync + 'static, String> {
    let response = ureq::post(format!("{TTS_URL}/synthesize")).config().timeout_recv_response(Some(std::time::Duration::from_secs(30))).timeout_recv_body(Some(std::time::Duration::from_secs(600))).build().send_json(serde_json::json!({ "text": text, "voice": tuning.voice, "temp": tuning.temp, "eos_threshold": tuning.eos_threshold }))
        .map_err(|error| format!("Pocket TTS failed: {error}"))?;
    Ok(response.into_body().into_reader())
}

/// Ask the running model to load the voice before accepting a persistent change.
pub fn validate_voice(name: &str) -> Result<(), String> {
    ureq::post(format!("{TTS_URL}/voice")).config().timeout_global(Some(std::time::Duration::from_secs(30))).build()
        .send_json(serde_json::json!({ "voice": name })).map(|_| ())
        .map_err(|error| format!("Unable to load voice {name:?}: {error}. Use list_voices to choose an installed voice; your settings have not changed."))
}

/// Turns a WAV already in the media folder into a voice named `name`, via the speech server. Needs cloning enabled.
pub fn clone_voice(name: &str, wav: &std::path::Path) -> Result<(), String> {
    ureq::post(format!("{TTS_URL}/clone")).config().timeout_global(Some(std::time::Duration::from_secs(300))).http_status_as_error(false).build()
        .send_json(serde_json::json!({ "name": name, "audio": wav })).map_err(|error| format!("Voice cloning failed: {error}")).and_then(|mut response| {
            if response.status().is_success() { return Ok(()); }
            let body = response.body_mut().read_to_string().unwrap_or_default();
            Err(body.split("Error code explanation:").nth(1).and_then(|rest| rest.split("</p>").next()).map(|s| s.trim().trim_end_matches('.').to_string()).unwrap_or_else(|| "Voice cloning failed.".into()))
        })
}

/// Buffered roundtrip helper for the provisioning check; Discord uses synthesize_stream directly.
pub fn synthesize(text: &str) -> Result<Vec<i16>, String> {
    let mut bytes = Vec::new();
    synthesize_stream(text, &crate::tuning::current())?.read_to_end(&mut bytes).map_err(|error| format!("Pocket TTS stream failed: {error}"))?;
    if bytes.len() % 4 != 0 { return Err("Pocket TTS returned incomplete PCM.".into()); }
    let mono: Vec<i16> = bytes.chunks_exact(4).map(|sample| (f32::from_le_bytes(sample.try_into().unwrap()).clamp(-1.0, 1.0) * 32767.0) as i16).collect();
    Ok(pocket_to_discord(&mono))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downsamples_48k_stereo_to_16k_mono() {
        assert_eq!(downsample(&[0, 6, 0, 6, 0, 6, 100, 100, 100, 100, 100, 100, 7]), vec![3, 100], "a trailing partial frame is dropped");
    }

    #[test]
    fn wav_header_describes_16k_mono_16bit() {
        let wav = wav16k(&[1, -1]);
        assert_eq!((&wav[0..4], &wav[8..12], wav.len()), (&b"RIFF"[..], &b"WAVE"[..], 48));
        assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 16_000);
        assert_eq!(u32::from_le_bytes(wav[40..44].try_into().unwrap()), 4);
        assert_eq!(&wav[44..], &[1, 0, 255, 255]);
    }

    #[test]
    fn upsamples_pocket_audio_to_discord_stereo() {
        let out = pocket_to_discord(&vec![1000; 2400]);
        assert_eq!(out.len(), 4800 * 2, "0.1 s at 48 kHz stereo");
        assert!(out.iter().all(|&s| s == 1000) && out.chunks(2).all(|f| f[0] == f[1]));
        assert!(pocket_to_discord(&[]).is_empty());
    }

    #[test]
    fn strips_whisper_non_speech_markers() {
        assert_eq!(clean_transcript(" [BLANK_AUDIO] hello (music) there [ a [b] ] "), "hello there");
    }

}
