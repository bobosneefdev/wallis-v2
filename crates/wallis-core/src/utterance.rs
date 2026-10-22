//! Groups Discord's 20 ms voice ticks into utterances per speaker. The silence window is live-tunable.
use std::collections::HashMap;

/// One tick is 20 ms of 48 kHz stereo (960 frames * 2 channels).
pub const TICK_MS: u64 = 20;
/// Peak amplitude below this is room noise, not speech.
const MIN_AMPLITUDE: i16 = 500;
/// Shorter than this is a click or cough.
const MIN_SPEECH_MS: u64 = 200;

#[derive(Default)]
struct Capture { pcm: Vec<i16>, speech_samples: usize, silent_ms: u64 }

#[derive(Debug, PartialEq)]
pub struct Utterance { pub speaker: u32, pub pcm: Vec<i16> }

#[derive(Default)]
pub struct Segmenter(HashMap<u32, Capture>);

impl Segmenter {
    /// Feeds one tick for every speaker heard (`speaking`) and every known-but-quiet speaker (`silent`).
    /// Missing packets and decoded quiet PCM both count toward the silence window. Returns finished utterances.
    pub fn tick(&mut self, speaking: &[(u32, &[i16])], _silent: &[u32]) -> Vec<Utterance> {
        for capture in self.0.values_mut() { capture.silent_ms += TICK_MS; }
        for (speaker, pcm) in speaking {
            let peak = pcm.iter().map(|s| s.saturating_abs()).max().unwrap_or(0);
            if peak < MIN_AMPLITUDE && !self.0.contains_key(speaker) { continue; }
            let capture = self.0.entry(*speaker).or_default();
            if peak >= MIN_AMPLITUDE { capture.silent_ms = 0; }
            capture.pcm.extend_from_slice(pcm);
            if peak >= MIN_AMPLITUDE { capture.speech_samples = capture.pcm.len(); }
        }
        let mut done = vec![];
        // Discord can keep sending decoded PCM during a pause. End on acoustic silence,
        // not only when the sender stops transmitting packets; count each speaker once.
        let silence_ms = crate::tuning::current().silence_ms as u64;
        let quiet: Vec<_> = self.0.iter().filter(|(_, capture)| capture.silent_ms >= silence_ms).map(|(&speaker, _)| speaker).collect();
        for speaker in quiet {
            let capture = self.0.remove(&speaker).expect("capture exists");
            let speech_ms = capture.speech_samples as u64 / 2 * 1000 / 48_000;
            if speech_ms >= MIN_SPEECH_MS { done.push(Utterance { speaker, pcm: capture.pcm }); }
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
    fn decoded_quiet_packets_end_speech_without_waiting_for_discord_vad() {
        let mut segmenter = Segmenter::default();
        for _ in 0..20 { assert!(segmenter.tick(&[(7, &LOUD)], &[]).is_empty()); }
        let ticks = (crate::tuning::current().silence_ms as u64).div_ceil(TICK_MS);
        for _ in 1..ticks { assert!(segmenter.tick(&[(7, &QUIET)], &[]).is_empty()); }
        let out = segmenter.tick(&[(7, &QUIET)], &[]);
        assert_eq!((out.len(), out[0].speaker), (1, 7));
        assert!(segmenter.tick(&[(7, &QUIET)], &[]).is_empty());
        assert!(segmenter.0.is_empty(), "idle microphones must not accumulate audio");
        for _ in 0..3 { segmenter.tick(&[(7, &LOUD)], &[]); }
        for _ in 0..ticks { assert!(segmenter.tick(&[(7, &QUIET)], &[]).is_empty(), "trailing quiet must not turn a click into speech"); }
    }

    #[test]
    fn missing_packets_count_as_one_silent_tick() {
        let mut segmenter = Segmenter::default();
        segmenter.tick(&[(7, &LOUD)], &[]);
        segmenter.tick(&[], &[7]);
        assert_eq!(segmenter.0[&7].silent_ms, TICK_MS);
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
