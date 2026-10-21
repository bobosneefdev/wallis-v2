use serde::{Deserialize, Serialize};
use std::path::Path;
use crate::provision::{STT_MODELS, TTS_VOICES};
pub const FIRST_NAME: &str = "Wallis";
pub const PROFILE_FILE: &str = "profile.json";
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct WallisConfig {
    pub discord_application_id: String,
    pub discord_allowed_users: String,
    pub discord_read_channels: String,
    pub discord_write_channels: String,
    pub discord_voice_text_channel: String,
    pub voice_participation: String,
    pub text_participation: String,
    pub participation_cooldown_secs: u32,
    pub auto_start: bool,
    pub onboarded: bool,
    pub stt_model: String,
    pub stt_threads: u32,
    pub tts_voice: String,
    pub tts_speed: f32,
    pub silence_ms: u32,
}

impl Default for WallisConfig {
    fn default() -> Self {
        Self { discord_application_id: String::new(), discord_allowed_users: String::new(), discord_read_channels: String::new(), discord_write_channels: String::new(), discord_voice_text_channel: String::new(), voice_participation: "natural".into(), text_participation: "natural".into(), participation_cooldown_secs: 5, auto_start: false, onboarded: false, stt_model: "parakeet-0.6b-v3-int8".into(), stt_threads: 4, tts_voice: "emily".into(), tts_speed: 1.0, silence_ms: 350 }
    }
}

/// Wallis' own dating-profile-style identity, written once by the realtime agent and kept in `profile.json`.
#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    #[serde(default)] pub name: String,
    #[serde(default)] pub age: String,
    #[serde(default)] pub pronouns: String,
    #[serde(default)] pub location: String,
    #[serde(default)] pub occupation: String,
    #[serde(default)] pub tagline: String,
    #[serde(default)] pub bio: String,
    #[serde(default)] pub interests: Vec<String>,
    #[serde(default)] pub looking_for: String,
    #[serde(default)] pub fun_fact: String,
}

pub fn load_config_file(path: &Path) -> Result<WallisConfig, String> {
    if !path.is_file() { return Ok(WallisConfig::default()); }
    let mut config: WallisConfig = serde_json::from_slice(&std::fs::read(path).map_err(|error| format!("Unable to read Wallis configuration: {error}"))?)
        .map_err(|error| format!("Wallis configuration is invalid: {error}"))?;
    if ["tiny.en", "base.en", "small.en", "parakeet-110m-int8"].contains(&config.stt_model.as_str()) { config.stt_model = "parakeet-0.6b-v3-int8".into(); }
    if !TTS_VOICES.iter().any(|(id, _)| *id == config.tts_voice) { config.tts_voice = "emily".into(); config.tts_speed = 1.0; }
    Ok(config)
}

pub fn save_config_file(path: &Path, config: &WallisConfig) -> Result<(), String> {
    let parent = path.parent().ok_or("Unable to determine the Wallis configuration folder.")?;
    std::fs::create_dir_all(parent).map_err(|error| format!("Unable to create the Wallis configuration folder: {error}"))?;
    let temporary = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(config).map_err(|error| format!("Unable to serialize Wallis configuration: {error}"))?;
    std::fs::write(&temporary, bytes).map_err(|error| format!("Unable to write Wallis configuration: {error}"))?;
    let result = std::fs::rename(&temporary, path).map_err(|error| format!("Unable to replace Wallis configuration: {error}"));
    let _ = std::fs::remove_file(temporary);
    result
}

pub fn validate_allowed_users(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() || !value.split(',').all(|id| id == "*" || (10..=25).contains(&id.len()) && id.bytes().all(|byte| byte.is_ascii_digit())) {
        return Err("Add Discord user IDs or * for anyone to the allow-list.".into());
    }
    Ok(value.to_string())
}


/// The first name is never configurable: anything Wallis or a model writes is forced back to it.
pub fn lock_first_name(value: &str) -> String {
    let cleaned: String = value.chars().map(|character| if character.is_control() { ' ' } else { character }).collect();
    let rest = cleaned.split_whitespace().skip(1).collect::<Vec<_>>().join(" ");
    let name = if rest.is_empty() { FIRST_NAME.to_string() } else { format!("{FIRST_NAME} {rest}") };
    name.chars().take(80).collect()
}

pub fn sanitize_profile(mut profile: Profile) -> Profile {
    profile.name = lock_first_name(&profile.name);
    profile.interests.truncate(12);
    profile
}

/// Voice knobs come from the GUI, so clamp them to what the runtimes actually accept.
pub fn validate_voice(config: &mut WallisConfig) -> Result<(), String> {
    if !STT_MODELS.iter().any(|(id, ..)| *id == config.stt_model) { return Err("Choose a supported speech recognition model.".into()); }
    if !TTS_VOICES.iter().any(|(id, _)| *id == config.tts_voice) { return Err("Choose a supported voice.".into()); }
    if !(1..=64).contains(&config.stt_threads) { return Err("Use between 1 and 64 speech recognition threads.".into()); }
    if !(0.5..=2.0).contains(&config.tts_speed) { return Err("Voice speed must be between 0.5 and 2.0.".into()); }
    if !(100..=2000).contains(&config.silence_ms) { return Err("The silence window must be between 100 and 2000 milliseconds.".into()); }
    Ok(())
}

pub fn validate_discord_channels(config: &mut WallisConfig) -> Result<(), String> {
    for channels in [&mut config.discord_read_channels, &mut config.discord_write_channels] {
        *channels = if channels.trim().is_empty() { String::new() } else {
            validate_allowed_users(channels).map_err(|_| "Channel lists must contain numeric Discord channel IDs separated by commas.".to_string())?
        };
    }
    config.discord_voice_text_channel = config.discord_voice_text_channel.trim().to_string();
    if !config.discord_voice_text_channel.is_empty() && !config.discord_write_channels.split(',').any(|id| id == config.discord_voice_text_channel) {
        return Err("The voice output channel must be in the write channel list.".into());
    }
    if ![&config.voice_participation, &config.text_participation].iter().all(|mode| matches!(mode.as_str(), "mentions" | "natural")) {
        return Err("Choose a supported participation mode.".into());
    }
    if !(5..=300).contains(&config.participation_cooldown_secs) { return Err("Use 5–300 seconds between unprompted contributions.".into()); }
    Ok(())
}

pub fn extract_json(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let (mut depth, mut in_string, mut escaped) = (0usize, false, false);
    for (index, byte) in text.bytes().enumerate().skip(start) {
        if in_string {
            match byte { _ if escaped => escaped = false, b'\\' => escaped = true, b'"' => in_string = false, _ => {} }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => { depth -= 1; if depth == 0 { return text.get(start..=index); } }
            _ => {}
        }
    }
    None
}

pub fn read_profile(app_data: &Path) -> Result<Profile, String> {
    let text = std::fs::read_to_string(app_data.join(PROFILE_FILE)).map_err(|_| "Wallis has no profile yet.".to_string())?;
    serde_json::from_str(&text).map(sanitize_profile).map_err(|_| "Wallis' profile file is not valid JSON.".to_string())
}

/// Who the realtime agent is. Before a profile exists it is just the name.
pub fn persona(app_data: &Path) -> String {
    let Ok(p) = read_profile(app_data) else { return format!("You are {FIRST_NAME}.") };
    format!("# {name}\n\nYou are {name}. This is who you are, not a role you play.\n\n- Age: {age}\n- Pronouns: {pronouns}\n- Lives in: {location}\n- Does: {occupation}\n- In one line: {tagline}\n- Into: {interests}\n- Looking for: {looking_for}\n- Fun fact: {fun_fact}\n\n{bio}\n\nYour first name is always {first}.",
        name = p.name, age = p.age, pronouns = p.pronouns, location = p.location, occupation = p.occupation, tagline = p.tagline, interests = p.interests.join(", "), looking_for = p.looking_for, fun_fact = p.fun_fact, bio = p.bio, first = FIRST_NAME)
}

/// The sanitized profile as JSON, for the `/profile` embed. `None` until one has been generated.
pub fn profile_json(app_data: &Path) -> Option<serde_json::Value> { read_profile(app_data).ok().and_then(|profile| serde_json::to_value(profile).ok()) }

/// The user turn of the one-off generator: a random seed plus the optional preferences. The instructions live in `prompts::profile_generator_prompt`.
pub fn generation_prompt(preferences: &str) -> String {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|elapsed| elapsed.as_nanos()).unwrap_or_default();
    let preferences = preferences.trim();
    format!(
        "Generate the profile now. Randomness seed {seed:x}: let it push you somewhere unexpected, not the safest choice.{preferences}",
        preferences = if preferences.is_empty() { String::new() } else { format!("\n\nPreferences to take into account (build around them where they fit, and choose everything else yourself):\n{preferences}") },
        seed = nanos ^ u128::from(std::process::id()).wrapping_mul(0x9E37_79B9_7F4A_7C15),
    )
}


#[derive(Default, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Roles { pub realtime: crate::role::RoleConfig, pub worker: crate::role::RoleConfig }
