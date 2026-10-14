//! Groups Discord's 20 ms voice ticks into utterances per speaker. Same thresholds as the Node bot.
use std::collections::HashMap;

/// One tick is 20 ms of 48 kHz stereo (960 frames * 2 channels).
pub const TICK_MS: u64 = 20;
/// Silence that ends an utterance.
const SILENCE_MS: u64 = 150;
/// Peak amplitude below this is room noise, not speech.
const MIN_AMPLITUDE: i16 = 500;
/// Shorter than this is a click or cough.
const MIN_SPEECH_MS: u64 = 200;

#[derive(Default)]
struct Capture { pcm: Vec<i16>, silent_ms: u64, peak: i16 }

#[derive(Debug, PartialEq)]
pub struct Utterance { pub speaker: u32, pub pcm: Vec<i16> }

#[derive(Default)]
pub struct Segmenter(HashMap<u32, Capture>);

impl Segmenter {
    /// Feeds one tick for every speaker heard (`speaking`) and every known-but-quiet speaker (`silent`).
    /// Returns the utterances that just ended.
    pub fn tick(&mut self, speaking: &[(u32, &[i16])], silent: &[u32]) -> Vec<Utterance> {
        for (speaker, pcm) in speaking {
            let capture = self.0.entry(*speaker).or_default();
            capture.silent_ms = 0;
            capture.peak = capture.peak.max(pcm.iter().map(|s| s.saturating_abs()).max().unwrap_or(0));
            capture.pcm.extend_from_slice(pcm);
        }
        let mut done = vec![];
        for speaker in silent.iter().copied().chain(self.0.keys().copied().filter(|id| !speaking.iter().any(|(s, _)| s == id)).collect::<Vec<_>>()) {
            let Some(capture) = self.0.get_mut(&speaker) else { continue };
            capture.silent_ms += TICK_MS;
            if capture.silent_ms >= SILENCE_MS {
                let capture = self.0.remove(&speaker).expect("capture exists");
                let speech_ms = capture.pcm.len() as u64 / 2 * 1000 / 48_000;
                if capture.peak >= MIN_AMPLITUDE && speech_ms >= MIN_SPEECH_MS { done.push(Utterance { speaker, pcm: capture.pcm }); }
            }
        }
        done
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOUD: [i16; 1920] = [2000; 1920];
    const QUIET: [i16; 1920] = [10; 1920];

    fn run(segmenter: &mut Segmenter, speaking_ticks: usize, level: &[i16]) -> Vec<Utterance> {
        for _ in 0..speaking_ticks { assert!(segmenter.tick(&[(7, level)], &[]).is_empty()); }
        (0..8).flat_map(|_| segmenter.tick(&[], &[7])).collect()
    }

    #[test]
    fn ends_an_utterance_after_trailing_silence() {
        let out = run(&mut Segmenter::default(), 20, &LOUD);
        assert_eq!((out.len(), out[0].speaker, out[0].pcm.len()), (1, 7, 20 * 1920));
    }

    #[test]
    fn drops_noise_and_clicks() {
        assert!(run(&mut Segmenter::default(), 20, &QUIET).is_empty(), "below the amplitude floor");
        assert!(run(&mut Segmenter::default(), 3, &LOUD).is_empty(), "shorter than 200 ms");
    }

    #[test]
    fn a_pause_inside_speech_does_not_split_it() {
        let mut segmenter = Segmenter::default();
        for _ in 0..10 { segmenter.tick(&[(7, &LOUD)], &[]); }
        for _ in 0..3 { assert!(segmenter.tick(&[], &[7]).is_empty()); }
        for _ in 0..10 { segmenter.tick(&[(7, &LOUD)], &[]); }
        let out: Vec<_> = (0..8).flat_map(|_| segmenter.tick(&[], &[7])).collect();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].pcm.len(), 20 * 1920);
    }
}
