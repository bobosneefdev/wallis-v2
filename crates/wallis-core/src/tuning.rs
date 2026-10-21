//! Live-tunable voice and conversation knobs. Wallis changes them mid-conversation (`tune`), they persist in
//! `tuning.json`, and every reader asks `current()` so a change applies on the next sentence, tick or turn without a restart.
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::RwLock;
use crate::provision::{EMOTIONS, TTS_VOICES};

pub const FILE: &str = "tuning.json";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Tuning {
    /// One of the four speakers (`TTS_VOICES`).
    pub voice: String,
    /// How he sounds: one of `EMOTIONS`.
    pub emotion: String,
    /// Playback speed, 0.5–2.0 (also raises pitch).
    pub speed: f32,
    /// Sampling temperature: higher is livelier and more varied, lower is flatter and steadier.
    pub temp: f32,
    /// Seconds between unprompted contributions.
    pub cooldown_secs: u32,
    /// Seconds after an answer in which the same person's voice follow-up needs no name.
    pub follow_up_secs: u32,
    /// Silence that ends someone's turn, so shorter means he answers faster and talks over pauses more.
    pub silence_ms: u32,
    /// A person talking this long over him cuts him off. 0 means they never do; he stops only when he decides to.
    pub barge_in_ms: u32,
}

impl Default for Tuning {
    fn default() -> Self { Self { voice: "emily".into(), emotion: "neutral".into(), speed: 1.0, temp: 1.0, cooldown_secs: 5, follow_up_secs: 20, silence_ms: 150, barge_in_ms: 0 } }
}

impl Tuning {
    /// Every value comes from a model, so validate it before applying it.
    pub fn validate(&self) -> Result<(), String> {
        let ranges = [("speed", self.speed, 0.5, 2.0), ("temp", self.temp, 0.3, 1.5)];
        for (name, value, low, high) in ranges { if !(low..=high).contains(&value) { return Err(format!("{name} must be between {low} and {high}.")); } }
        let counts = [("cooldown_secs", self.cooldown_secs, 5, 300), ("follow_up_secs", self.follow_up_secs, 0, 120), ("silence_ms", self.silence_ms, 100, 2000), ("barge_in_ms", self.barge_in_ms, 0, 5000)];
        for (name, value, low, high) in counts { if !(low..=high).contains(&value) { return Err(format!("{name} must be between {low} and {high}.")); } }
        if !TTS_VOICES.iter().any(|v| v.0 == self.voice) { return Err(format!("voice must be one of: {}.", TTS_VOICES.map(|v| v.0).join(", "))); }
        if !EMOTIONS.contains(&self.emotion.as_str()) { return Err(format!("emotion must be one of: {}.", EMOTIONS.join(", "))); }
        Ok(())
    }

    /// Applies the named changes from a tool call; unknown keys are an error so a typo cannot silently do nothing.
    pub fn apply(&mut self, changes: &serde_json::Map<String, serde_json::Value>) -> Result<Vec<&'static str>, String> {
        let mut next = self.clone();
        let mut changed = Vec::new();
        for (key, value) in changes {
            let float = || value.as_f64().map(|n| n as f32).ok_or_else(|| format!("{key} must be a number."));
            let count = || value.as_u64().and_then(|n| u32::try_from(n).ok()).ok_or_else(|| format!("{key} must be a whole number."));
            match key.as_str() {
                "voice" => { next.voice = value.as_str().ok_or("voice must be text.")?.into(); changed.push("voice"); }
                "emotion" => { next.emotion = value.as_str().ok_or("emotion must be text.")?.into(); changed.push("emotion"); }
                "speed" => { next.speed = float()?; changed.push("speed"); }
                "temp" => { next.temp = float()?; changed.push("temp"); }
                "cooldownSecs" => { next.cooldown_secs = count()?; changed.push("cooldownSecs"); }
                "followUpSecs" => { next.follow_up_secs = count()?; changed.push("followUpSecs"); }
                "silenceMs" => { next.silence_ms = count()?; changed.push("silenceMs"); }
                "bargeInMs" => { next.barge_in_ms = count()?; changed.push("bargeInMs"); }
                "channelId" => {}
                other => return Err(format!("Unknown setting {other:?}.")),
            }
        }
        if changed.is_empty() { return Err("Nothing to change.".into()); }
        next.validate()?;
        *self = next;
        Ok(changed)
    }
}

static CURRENT: RwLock<Option<Tuning>> = RwLock::new(None);
#[cfg(test)]
pub(crate) static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The live values. Before `load` they are the defaults.
pub fn current() -> Tuning { CURRENT.read().ok().and_then(|t| t.clone()).unwrap_or_default() }

/// Loads `tuning.json` from the data dir and makes it current. `seed` is the desktop's voice settings: when the user
/// changes those, the seed differs from the one last used and wins over Wallis' own earlier tuning; otherwise his tuning survives restarts.
pub fn load(dir: &Path, seed: Tuning) -> Tuning {
    let marker = dir.join("tuning.seed");
    let seed_json = serde_json::to_string(&seed).unwrap_or_default();
    let fresh = std::fs::read_to_string(&marker).ok().as_deref() != Some(&seed_json);
    if fresh { let _ = std::fs::write(&marker, &seed_json); let _ = std::fs::remove_file(dir.join(FILE)); }
    let tuning = std::fs::read_to_string(dir.join(FILE)).ok().and_then(|text| serde_json::from_str::<Tuning>(&text).ok()).filter(|t| t.validate().is_ok()).unwrap_or(seed);
    if let Ok(mut slot) = CURRENT.write() { *slot = Some(tuning.clone()); }
    tuning
}

/// Applies `changes`, saves them and makes them current.
pub fn update(dir: &Path, changes: &serde_json::Map<String, serde_json::Value>) -> Result<(Tuning, Vec<&'static str>), String> {
    let mut slot = CURRENT.write().map_err(|_| "Tuning is unavailable.".to_string())?;
    let mut tuning = slot.clone().unwrap_or_default();
    let changed = tuning.apply(changes)?;
    save(dir, &tuning)?;
    *slot = Some(tuning.clone());
    Ok((tuning, changed))
}

fn save(dir: &Path, tuning: &Tuning) -> Result<(), String> {
    let temporary = dir.join(format!("{FILE}.tmp"));
    std::fs::write(&temporary, serde_json::to_vec_pretty(tuning).map_err(|e| e.to_string())?).and_then(|_| std::fs::rename(&temporary, dir.join(FILE))).map_err(|error| format!("Unable to save the tuning: {error}"))
}

/// Reset only failed speech settings. A late error must not overwrite a newer tune.
/// Recovery takes effect even if the disk is full or unavailable.
pub fn recover(dir: &Path, failed: &Tuning) -> Result<(), String> {
    let mut slot = CURRENT.write().map_err(|_| "Tuning is unavailable.".to_string())?;
    let mut next = slot.clone().unwrap_or_default();
    if (next.voice.as_str(), next.emotion.as_str(), next.speed, next.temp) != (failed.voice.as_str(), failed.emotion.as_str(), failed.speed, failed.temp) { return Ok(()); }
    let seed = std::fs::read_to_string(dir.join("tuning.seed")).ok().and_then(|s| serde_json::from_str::<Tuning>(&s).ok()).filter(|t| t.validate().is_ok()).unwrap_or_default();
    next.voice = seed.voice;
    next.emotion = Tuning::default().emotion;
    next.speed = 1.0;
    next.temp = Tuning::default().temp;
    *slot = Some(next.clone());
    save(dir, &next)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn changes(value: serde_json::Value) -> serde_json::Map<String, serde_json::Value> { value.as_object().unwrap().clone() }

    #[test]
    fn applies_valid_changes_atomically_and_rejects_the_rest() {
        let mut t = Tuning::default();
        assert_eq!(t.apply(&changes(json!({"speed":1.2,"temp":0.6,"bargeInMs":600,"emotion":"happy","voice":"paul"}))).unwrap().len(), 5);
        assert_eq!((t.speed, t.temp, t.barge_in_ms, t.emotion.as_str(), t.voice.as_str()), (1.2, 0.6, 600, "happy", "paul"));
        let before = t.clone();
        for bad in [json!({"speed":3.0}), json!({"speed":1.1,"silenceMs":5}), json!({"voice":"../x"}), json!({"emotion":"sleepy"}), json!({"nope":1}), json!({}), json!({"cooldownSecs":-1})] {
            assert!(t.apply(&changes(bad)).is_err());
            assert_eq!(t, before, "a rejected call changes nothing");
        }
    }

    #[test]
    fn updates_persist_and_are_current() {
        let _guard = TEST_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("wallis-tuning-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        load(&dir, Tuning::default());
        update(&dir, &changes(json!({"speed":1.1}))).unwrap();
        let before = std::fs::read(dir.join(FILE)).unwrap();
        assert!(update(&dir, &changes(json!({"voice":"../invalid","speed":1.9}))).is_err());
        assert_eq!(std::fs::read(dir.join(FILE)).unwrap(), before, "a rejected voice must not change persisted settings");
        assert_eq!(current().speed, 1.1);
        assert_eq!(load(&dir, Tuning::default()).speed, 1.1, "read back from disk");
        assert_eq!(load(&dir, Tuning { speed: 0.9, ..Tuning::default() }).speed, 0.9, "a changed desktop setting wins");
        let failed = Tuning { voice: "missing".into(), emotion: "angry".into(), temp: 1.5, speed: 2.0, cooldown_secs: 42, ..Default::default() };
        save(&dir, &failed).unwrap();
        assert_eq!(load(&dir, Tuning { speed: 0.9, ..Tuning::default() }).voice, "emily", "an invalid saved voice cannot disable speech on startup");
        *CURRENT.write().unwrap() = Some(failed.clone());
        recover(&dir, &failed).unwrap();
        let recovered = current();
        assert_eq!((recovered.voice.as_str(), recovered.emotion.as_str(), recovered.speed, recovered.temp, recovered.cooldown_secs), ("emily", "neutral", 1.0, 1.0, 42));
        assert_eq!(load(&dir, Tuning { speed: 0.9, ..Tuning::default() }), recovered, "recovery survives restart");
        update(&dir, &changes(json!({"temp":0.6}))).unwrap();
        recover(&dir, &failed).unwrap();
        assert_eq!(current().temp, 0.6, "late playback errors must not overwrite newer settings");
        let live = current();
        std::fs::remove_dir_all(dir).unwrap();
        assert!(recover(&std::env::temp_dir().join("wallis-no-such-recovery-directory").join("child"), &live).is_err());
        assert_eq!(current().temp, 1.0, "disk failure cannot prevent live recovery");
    }
}
