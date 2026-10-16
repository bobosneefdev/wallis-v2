//! Live-tunable voice and conversation knobs. Wallis changes them mid-conversation (`tune`), they persist in
//! `tuning.json`, and every reader asks `current()` so a change applies on the next sentence, tick or turn without a restart.
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::RwLock;

pub const FILE: &str = "tuning.json";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Tuning {
    /// Preset or cloned voice name.
    pub voice: String,
    /// Playback speed, 0.5–2.0 (also raises pitch).
    pub speed: f32,
    /// Pocket sampling temperature: higher is livelier and more varied, lower is flatter and steadier.
    pub temp: f32,
    /// Pocket end-of-speech threshold: higher ends sentences sooner, lower lets them trail.
    pub eos_threshold: f32,
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
    fn default() -> Self { Self { voice: "alba".into(), speed: 1.0, temp: 0.3, eos_threshold: -4.0, cooldown_secs: 5, follow_up_secs: 20, silence_ms: 150, barge_in_ms: 0 } }
}

impl Tuning {
    /// Every value comes from a model, so it is clamped, never trusted.
    pub fn validate(&self) -> Result<(), String> {
        let ranges = [("speed", self.speed, 0.5, 2.0), ("temp", self.temp, 0.05, 1.5), ("eos_threshold", self.eos_threshold, -8.0, 0.0)];
        for (name, value, low, high) in ranges { if !(low..=high).contains(&value) { return Err(format!("{name} must be between {low} and {high}.")); } }
        let counts = [("cooldown_secs", self.cooldown_secs, 5, 300), ("follow_up_secs", self.follow_up_secs, 0, 120), ("silence_ms", self.silence_ms, 100, 2000), ("barge_in_ms", self.barge_in_ms, 0, 5000)];
        for (name, value, low, high) in counts { if !(low..=high).contains(&value) { return Err(format!("{name} must be between {low} and {high}.")); } }
        if self.voice.is_empty() || !self.voice.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-') || self.voice.len() > 40 { return Err("Voice names use letters, digits, - and _ (up to 40).".into()); }
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
                "speed" => { next.speed = float()?; changed.push("speed"); }
                "temp" => { next.temp = float()?; changed.push("temp"); }
                "eosThreshold" => { next.eos_threshold = float()?; changed.push("eosThreshold"); }
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
    let temporary = dir.join(format!("{FILE}.tmp"));
    std::fs::write(&temporary, serde_json::to_vec_pretty(&tuning).map_err(|e| e.to_string())?).and_then(|_| std::fs::rename(&temporary, dir.join(FILE))).map_err(|error| format!("Unable to save the tuning: {error}"))?;
    *slot = Some(tuning.clone());
    Ok((tuning, changed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn changes(value: serde_json::Value) -> serde_json::Map<String, serde_json::Value> { value.as_object().unwrap().clone() }

    #[test]
    fn applies_valid_changes_atomically_and_rejects_the_rest() {
        let mut t = Tuning::default();
        assert_eq!(t.apply(&changes(json!({"speed":1.2,"temp":0.6,"bargeInMs":600}))).unwrap().len(), 3);
        assert_eq!((t.speed, t.temp, t.barge_in_ms), (1.2, 0.6, 600));
        let before = t.clone();
        for bad in [json!({"speed":3.0}), json!({"speed":1.1,"silenceMs":5}), json!({"voice":"../x"}), json!({"nope":1}), json!({}), json!({"cooldownSecs":-1})] {
            assert!(t.apply(&changes(bad)).is_err());
            assert_eq!(t, before, "a rejected call changes nothing");
        }
    }

    #[test]
    fn updates_persist_and_are_current() {
        let dir = std::env::temp_dir().join(format!("wallis-tuning-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        load(&dir, Tuning::default());
        update(&dir, &changes(json!({"voice":"johnny","speed":1.1}))).unwrap();
        assert_eq!(current().voice, "johnny");
        assert_eq!(load(&dir, Tuning::default()).speed, 1.1, "read back from disk");
        assert_eq!(load(&dir, Tuning { speed: 0.9, ..Tuning::default() }).speed, 0.9, "a changed desktop setting wins");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
