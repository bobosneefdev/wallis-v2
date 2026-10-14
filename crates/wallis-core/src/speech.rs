//! Speech plumbing between Discord (48 kHz stereo PCM), whisper.cpp (16 kHz mono WAV) and Piper (22.05 kHz mono PCM).
//! The two servers are the same local HTTP services the Node bot used.
use std::io::Read;

pub const WHISPER_URL: &str = "http://127.0.0.1:8085";
pub const PIPER_URL: &str = "http://127.0.0.1:8086";
const PIPER_RATE: usize = 22_050;

/// Discord voice -> whisper: average each 3 stereo frames (48 kHz -> 16 kHz) into one mono sample.
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

/// Piper output -> Discord voice: linear-interpolate 22.05 kHz mono up to 48 kHz and duplicate to stereo.
pub fn piper_to_discord(mono: &[i16]) -> Vec<i16> {
    if mono.is_empty() { return vec![]; }
    let frames = mono.len() * 48_000 / PIPER_RATE;
    let mut out = Vec::with_capacity(frames * 2);
    for frame in 0..frames {
        let position = frame as f64 * PIPER_RATE as f64 / 48_000.0;
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

fn multipart(wav: &[u8], boundary: &str) -> Vec<u8> {
    let mut body = Vec::new();
    for (name, value) in [("response_format", "json"), ("no_timestamps", "true"), ("token_timestamps", "false")] {
        body.extend(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n").bytes());
    }
    body.extend(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"audio.wav\"\r\nContent-Type: audio/wav\r\n\r\n").bytes());
    body.extend(wav);
    body.extend(format!("\r\n--{boundary}--\r\n").bytes());
    body
}

/// Speech to text through the local whisper-server.
pub fn transcribe(pcm_48k_stereo: &[i16]) -> Result<String, String> {
    let boundary = "wallis-boundary-7f3a";
    let response: serde_json::Value = ureq::post(format!("{WHISPER_URL}/inference"))
        .header("Content-Type", format!("multipart/form-data; boundary={boundary}"))
        .send(&multipart(&wav16k(&downsample(pcm_48k_stereo)), boundary)).map_err(|error| format!("whisper-server failed: {error}"))?
        .body_mut().read_json().map_err(|error| format!("whisper-server sent unreadable output: {error}"))?;
    Ok(clean_transcript(response["text"].as_str().unwrap_or("")))
}

/// Text to speech through the local Piper server; returns 48 kHz stereo PCM ready for Discord.
pub fn synthesize(text: &str, length_scale: f32) -> Result<Vec<i16>, String> {
    let mut response = ureq::post(format!("{PIPER_URL}/synthesize")).send_json(serde_json::json!({ "text": text, "length_scale": length_scale }))
        .map_err(|error| format!("Piper failed: {error}"))?;
    let mut bytes = Vec::new();
    response.body_mut().as_reader().read_to_end(&mut bytes).map_err(|error| format!("Piper stream failed: {error}"))?;
    let mono: Vec<i16> = bytes.chunks_exact(2).map(|pair| i16::from_le_bytes([pair[0], pair[1]])).collect();
    Ok(piper_to_discord(&mono))
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
    fn upsamples_piper_audio_to_discord_stereo() {
        let out = piper_to_discord(&vec![1000; 2205]);
        assert_eq!(out.len(), 4800 * 2, "0.1 s at 48 kHz stereo");
        assert!(out.iter().all(|&s| s == 1000) && out.chunks(2).all(|f| f[0] == f[1]));
        assert!(piper_to_discord(&[]).is_empty());
    }

    #[test]
    fn strips_whisper_non_speech_markers() {
        assert_eq!(clean_transcript(" [BLANK_AUDIO] hello (music) there [ a [b] ] "), "hello there");
    }

    #[test]
    fn multipart_body_carries_the_wav_and_closes_the_boundary() {
        let body = String::from_utf8_lossy(&multipart(b"WAVDATA", "b")).to_string();
        assert!(body.contains("name=\"file\"; filename=\"audio.wav\"") && body.contains("WAVDATA\r\n--b--\r\n") && body.contains("name=\"response_format\""));
    }
}
