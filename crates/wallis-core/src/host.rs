//! Executes the tool calls the realtime agent makes. Discord itself sits behind `Discord` so this tests without a bot.
use crate::jobs::Jobs;
use crate::pi::HostCall;
use crate::schedule::{self, Reminders};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub trait Discord {
    fn scope(&mut self, _guild: u64, _user: u64) {}
    fn say(&mut self, text: &str) -> Result<(), String>;
    /// Stops current and queued speech when Wallis chooses to interrupt himself.
    fn stop_speaking(&mut self) -> Result<(), String> { Err("Only the live Wallis can stop speaking.".into()) }
    fn message(&mut self, channel_id: &str, text: &str) -> Result<(), String>;
    fn file(&mut self, channel_id: &str, filename: &str, content: &str, text: &str) -> Result<(), String>;
    fn queue(&mut self, _path: &Path, _title: &str) -> Result<(), String> { Err("Music unavailable.".into()) }
    fn queue_list(&mut self) -> Result<String, String> { Err("Music unavailable.".into()) }
    fn skip_audio(&mut self) -> Result<(), String> { Err("Music unavailable.".into()) }
    fn stop_audio(&mut self) -> Result<(), String> { Err("Music unavailable.".into()) }
    fn volume(&mut self, _percent: u32) -> Result<(), String> { Err("Music unavailable.".into()) }
    /// Joins `channel`, or the voice channel of whoever Wallis is talking to. Returns the channel joined.
    fn join(&mut self, _channel: Option<u64>) -> Result<u64, String> { Err("Only the live Wallis can join calls.".into()) }
    /// Leaves the call.
    fn leave(&mut self) -> Result<(), String> { Err("Only the live Wallis can leave calls.".into()) }
    /// Recent messages of a channel he may read, oldest first, one per line.
    fn history(&mut self, _channel_id: &str, _limit: u8) -> Result<String, String> { Err("Only the live Wallis can read channels.".into()) }
    /// Sentences already queued to be spoken.
    fn pending_speech(&self) -> usize { 0 }
    /// The voice channel Wallis is in right now.
    fn voice_channel(&self) -> Option<u64> { None }
}

pub struct Host<'a, D: Discord> {
    pub discord: &'a mut D,
    pub jobs: &'a Jobs,
    pub reminders: &'a Reminders,
    /// Where this turn happens: reminders are scoped to the guild and remember who he was talking to.
    pub guild: u64,
    pub user: u64,
    /// Channels the user allowed Wallis to write to. Anything else is refused.
    pub write_channels: Vec<String>,
    /// Set when the agent chose to stay silent this turn.
    pub skipped: bool,
    /// `profile.json`. Only Wallis' own `update_profile` call writes it; the UI never edits it after creation.
    pub profile: PathBuf,
}

/// Sentences that may wait to be spoken before `say` pushes back.
pub const MAX_QUEUED_SPEECH: usize = 4;

/// The profile fields Wallis may change. The name is not among them, and readers force his first name anyway.
const PROFILE_FIELDS: [&str; 8] = ["age", "pronouns", "location", "occupation", "tagline", "bio", "lookingFor", "funFact"];

fn update_profile(path: &Path, changes: &Value) -> Result<String, String> {
    let mut profile: Value = std::fs::read_to_string(path).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_else(|| json!({}));
    let map = profile.as_object_mut().ok_or("The profile file is not an object.")?;
    let mut changed = Vec::new();
    for field in PROFILE_FIELDS {
        if let Some(text) = changes.get(field).and_then(Value::as_str) { map.insert(field.into(), json!(text.chars().take(1000).collect::<String>())); changed.push(field); }
    }
    if let Some(list) = changes.get("interests").and_then(Value::as_array) {
        map.insert("interests".into(), json!(list.iter().filter_map(Value::as_str).take(12).collect::<Vec<_>>()));
        changed.push("interests");
    }
    if changed.is_empty() { return Err("Nothing to change.".into()); }
    std::fs::write(path, serde_json::to_string_pretty(&profile).map_err(|e| e.to_string())?).map_err(|error| format!("Unable to save the profile: {error}"))?;
    Ok(format!("Updated {}. It applies from your next session.", changed.join(", ")))
}

impl<D: Discord> Host<'_, D> {
    fn data(&self) -> &Path { self.profile.parent().unwrap_or(Path::new(".")) }

    fn voices(&self) -> String {
        let names = |dir: PathBuf| std::fs::read_dir(dir).into_iter().flatten().filter_map(|e| e.ok()?.path().file_stem().map(|s| s.to_string_lossy().into_owned())).collect::<std::collections::BTreeSet<_>>();
        let (presets, cloned) = (crate::provision::TTS_VOICES.map(|v| v.0.to_string()).join(", "), names(self.data().join("voices")).into_iter().collect::<Vec<_>>().join(", "));
        format!("Presets: {presets}. Cloned: {}. Speaking now in: {}.", if cloned.is_empty() { "none".into() } else { cloned }, crate::tuning::current().voice)
    }

    /// Turns a WAV in the media folder (the worker prepares it) into a voice, and by default switches to it.
    fn clone_voice(&self, name: &str, path: &str, switch: bool) -> Result<String, String> {
        let name = name.trim().to_lowercase();
        if crate::provision::TTS_VOICES.iter().any(|v| v.0 == name) { return Err("That name belongs to a built-in voice; pick another.".into()); }
        if !self.data().join("cloning-enabled").is_file() { return Err("Voice cloning is not enabled. The host turns it on in the desktop app's voice settings.".into()); }
        let wav = crate::mods::confined_path(&self.data().join("media"), Path::new(path))?;
        if wav.extension().and_then(|e| e.to_str()) != Some("wav") { return Err("Pass a 16-bit WAV of the voice (a clean 10–30 s clip) from the media folder; ask the worker to prepare it.".into()); }
        crate::speech::clone_voice(&name, &wav)?;
        if switch { crate::tuning::update(self.data(), json!({"voice": name}).as_object().expect("object"))?; }
        Ok(format!("Cloned the voice as {name}{}.", if switch { " and switched to it from the next sentence" } else { "" }))
    }

    /// The returned string is the tool result the model sees, including failures (the model can recover from those).
    pub fn handle(&mut self, call: HostCall) -> String {
        let text = |key: &str| call.args.get(key).and_then(Value::as_str).unwrap_or("").to_string();
        let channel = text("channelId");
        let outcome = match call.tool.as_str() {
            "run_mod" => {
                let data = self.profile.parent().unwrap_or(Path::new("."));
                let input = json!({"options":call.args.get("options").cloned().unwrap_or(json!({})),"guild":self.guild.to_string(),"user":self.user.to_string(),"channel":Some(channel.clone()).filter(|c| self.write_channels.contains(c)).or_else(|| self.write_channels.first().cloned()).unwrap_or_default()});
                crate::mods::run(&data.to_path_buf(), &text("name"), input, &mut |call| self.handle(call))
            }
            "queue" => {
                let media = self.profile.parent().unwrap_or(Path::new(".")).join("media");
                crate::mods::confined_path(&media, Path::new(&text("path"))).and_then(|path| {
                    if !path.is_file() || std::fs::metadata(&path).map_err(|e| e.to_string())?.len() > 50 * 1024 * 1024 { return Err("Audio must be a file of at most 50 MiB.".into()); }
                    self.discord.queue(&path, &text("title"))
                }).map(|_| "Queued.".into())
            }
            "queue_list" => self.discord.queue_list(),
            "skip_audio" => self.discord.skip_audio().map(|_| "Skipped track.".into()),
            "stop_audio" => self.discord.stop_audio().map(|_| "Music stopped.".into()),
            "volume" => call.args.get("percent").and_then(Value::as_u64).filter(|p| *p <= 100).ok_or_else(|| "Volume must be 0–100.".to_string()).and_then(|p| self.discord.volume(p as u32)).map(|_| "Volume changed.".into()),
            // Speech is spoken in order, so a long queue means he is talking about the past. Make him catch up instead of piling on.
            "say" if self.discord.pending_speech() >= MAX_QUEUED_SPEECH => Err(format!("You already have {} sentences waiting to be spoken, so you are falling behind the conversation. Call stop_speaking, then say one short line that fits what is happening now.", self.discord.pending_speech())),
            "say" => self.discord.say(&text("text")).map(|_| "Queued for speech. Playback failures recover automatically.".to_string()),
            "stop_speaking" => self.discord.stop_speaking().map(|_| "Stopped speaking.".into()),
            "update_profile" => update_profile(&self.profile, &call.args),
            "tune" => call.args.as_object().ok_or_else(|| "Pass the settings to change.".to_string()).and_then(|changes| crate::tuning::update(self.data(), changes))
                .map(|(t, changed)| format!("Changed {}. Applies from the next sentence. Now: {}", changed.join(", "), serde_json::to_string(&t).unwrap_or_default())),
            "list_voices" => Ok(self.voices()),
            "clone_voice" => self.clone_voice(&text("name"), &text("path"), call.args.get("use").and_then(Value::as_bool).unwrap_or(true)),
            "channel_history" => {
                let limit = call.args.get("limit").and_then(Value::as_u64).unwrap_or(20).clamp(1, 50) as u8;
                self.discord.history(&channel, limit)
            }
            "join_call" => match call.args.get("channelId").filter(|value| !value.is_null()) {
                None => self.discord.join(None),
                Some(value) => value.as_str().and_then(|id| id.parse().ok()).ok_or_else(|| "`channelId` must be a voice channel id.".to_string()).and_then(|id| self.discord.join(Some(id))),
            }.map(|id| format!("Joined voice channel {id}. Say hello, or carry on where you left off.")),
            "leave_call" => self.discord.leave().map(|_| "Left the call.".to_string()),
            "remind" => call.args.get("minutes").and_then(Value::as_f64).ok_or_else(|| "`minutes` is required.".to_string())
                .and_then(|minutes| self.reminders.add(schedule::now(), minutes, &text("note"), self.guild, self.user, self.discord.voice_channel()))
                .map(|r| format!("Reminder {} set for {} (in {}). It will reach you as a message when it is due.", r.id, schedule::stamp(r.due_at), schedule::span(r.due_at - r.created_at))),
            "cancel_reminder" => match call.args.get("id").and_then(Value::as_i64) {
                Some(id) => self.reminders.cancel(id, self.guild).and_then(|found| if found { Ok(format!("Cancelled reminder {id}.")) } else { Err(format!("No pending reminder {id}.")) }),
                None => Err("`id` is required.".into()),
            },
            "list_reminders" => self.reminders.list(self.guild).map(|list| {
                if list.is_empty() { return "No reminders pending.".to_string() }
                let now = schedule::now();
                list.iter().map(|r| format!("{}: {} (in {}) {}", r.id, schedule::stamp(r.due_at), schedule::span(r.due_at - now), r.note)).collect::<Vec<_>>().join("\n")
            }),
            "skip" => { self.skipped = true; Ok("Staying silent.".into()) }
            "message" | "file" | "delegate" if !self.write_channels.contains(&channel) => Err(format!("Channel {channel:?} is not in the allowed write channels.")),
            "message" => self.discord.message(&channel, &text("text")).map(|_| "Posted.".into()),
            "file" => self.discord.file(&channel, &text("filename"), &text("content"), &text("text")).map(|_| "Attached.".into()),
            "delegate" => {
                let (task, context) = (text("task"), text("context"));
                if task.is_empty() { Err("`task` is required.".into()) } else {
                    self.jobs.enqueue_scoped(&format!("{task}\n\nContext:\n{context}"), &channel, self.guild, self.user).map(|id| format!("Queued as job {id}. The user does not see the worker: the full result comes back to you privately as a `[job finished]` message, and you tell them yourself. Say briefly that you are on it, then carry on."))
                }
            }
            other => Err(format!("Unknown tool {other:?}.")),
        };
        outcome.unwrap_or_else(|error| format!("Failed: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[derive(Default)]
    struct Fake { stopped: bool, spoken: Vec<String>, posted: Vec<(String, String)>, joined: Vec<Option<u64>>, left: bool, in_call: Option<u64>, music: Vec<String> }
    impl Discord for Fake {
        fn say(&mut self, text: &str) -> Result<(), String> { self.spoken.push(text.into()); Ok(()) }
        fn stop_speaking(&mut self) -> Result<(), String> { self.stopped = true; self.spoken.clear(); Ok(()) }
        fn message(&mut self, channel_id: &str, text: &str) -> Result<(), String> { self.posted.push((channel_id.into(), text.into())); Ok(()) }
        fn file(&mut self, _: &str, _: &str, _: &str, _: &str) -> Result<(), String> { Err("upload failed".into()) }
        fn join(&mut self, channel: Option<u64>) -> Result<u64, String> { self.joined.push(channel); self.in_call = Some(channel.unwrap_or(5)); Ok(channel.unwrap_or(5)) }
        fn leave(&mut self) -> Result<(), String> { self.left = true; self.in_call = None; Ok(()) }
        fn voice_channel(&self) -> Option<u64> { self.in_call }
        fn queue(&mut self, path: &Path, title: &str) -> Result<(), String> { self.music.push(format!("queue:{}:{title}", path.display())); Ok(()) }
        fn queue_list(&mut self) -> Result<String, String> { Ok(self.music.join("\n")) }
        fn skip_audio(&mut self) -> Result<(), String> { self.music.push("skip".into()); Ok(()) }
        fn stop_audio(&mut self) -> Result<(), String> { self.music.push("stop".into()); Ok(()) }
        fn volume(&mut self, percent: u32) -> Result<(), String> { self.music.push(format!("volume:{percent}")); Ok(()) }
    }

    fn call(tool: &str, args: Value) -> HostCall { HostCall { request_id: "1".into(), tool: tool.into(), args } }

    #[test]
    fn music_routes_controls_and_confines_downloads() {
        let root = std::env::temp_dir().join(format!("wallis-host-music-{}", std::process::id()));
        std::fs::create_dir_all(root.join("media")).unwrap();
        std::fs::write(root.join("media/song.webm"), "audio").unwrap();
        std::fs::write(root.join("outside.webm"), "private").unwrap();
        std::fs::File::create(root.join("media/large.webm")).unwrap().set_len(50 * 1024 * 1024 + 1).unwrap();
        let (mut fake, jobs, reminders) = (Fake::default(), Jobs::open(":memory:").unwrap(), Reminders::open(":memory:").unwrap());
        let mut host = Host { discord: &mut fake, jobs: &jobs, reminders: &reminders, guild: 1, user: 2, write_channels: vec!["10".into()], skipped: false, profile: root.join("profile.json") };
        for path in [root.join("outside.webm"), root.join("media/large.webm"), root.join("media")] {
            assert!(host.handle(call("queue", json!({"path":path,"title":"bad"}))).starts_with("Failed:"));
        }
        assert_eq!(host.handle(call("queue", json!({"path":"song.webm","title":"Song"}))), "Queued.");
        assert!(host.handle(call("queue_list", json!({}))).ends_with(":Song"));
        for percent in [json!(-1), json!(101), json!(1.5), json!("40")] {
            assert!(host.handle(call("volume", json!({"percent":percent}))).starts_with("Failed:"));
        }
        assert_eq!(host.handle(call("volume", json!({"percent":40}))), "Volume changed.");
        assert_eq!(host.handle(call("skip_audio", json!({}))), "Skipped track.");
        assert_eq!(host.handle(call("stop_audio", json!({}))), "Music stopped.");
        assert_eq!(fake.music, vec![format!("queue:{}:Song", root.join("media/song.webm").canonicalize().unwrap().display()), "volume:40".into(), "skip".into(), "stop".into()]);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn speech_continues_until_wallis_explicitly_stops_it() {
        let (mut fake, jobs, reminders) = (Fake::default(), Jobs::open(":memory:").unwrap(), Reminders::open(":memory:").unwrap());
        let mut host = Host { discord: &mut fake, jobs: &jobs, reminders: &reminders, guild: 1, user: 2, write_channels: vec![], skipped: false, profile: PathBuf::new() };
        host.handle(call("say", json!({ "text": "Keep going." })));
        host.handle(call("skip", json!({})));
        assert!(!host.discord.stopped);
        assert_eq!(host.discord.spoken, ["Keep going."]);
        assert_eq!(host.handle(call("stop_speaking", json!({}))), "Stopped speaking.");
        assert!(host.discord.stopped && host.discord.spoken.is_empty());
        host.handle(call("say", json!({ "text": "Changed my mind." })));
        assert_eq!(host.discord.spoken, ["Changed my mind."]);
    }

    #[test]
    fn routes_tools_enforces_write_channels_and_queues_delegations() {
        let (mut fake, jobs, reminders) = (Fake::default(), Jobs::open(":memory:").unwrap(), Reminders::open(":memory:").unwrap());
        let mut host = Host { discord: &mut fake, jobs: &jobs, reminders: &reminders, guild: 1, user: 2, write_channels: vec!["10".into()], skipped: false, profile: std::env::temp_dir().join(format!("wallis-host-profile-{}.json", std::process::id())) };
        assert!(host.handle(call("say", json!({ "text": "hi" }))).starts_with("Queued for speech."));
        assert_eq!(host.handle(call("message", json!({ "channelId": "10", "text": "yo" }))), "Posted.");
        assert!(host.handle(call("message", json!({ "channelId": "99", "text": "no" }))).starts_with("Failed: Channel"));
        assert!(host.handle(call("file", json!({ "channelId": "10", "filename": "a.rs", "content": "x" }))).contains("upload failed"));
        assert!(host.handle(call("delegate", json!({ "task": "fix it", "context": "repo is foo", "channelId": "10" }))).starts_with("Queued as job"));
        assert!(host.handle(call("delegate", json!({ "task": "x", "context": "", "channelId": "99" }))).starts_with("Failed"));
        assert!(host.handle(call("nope", json!({}))).contains("Unknown tool"));
        host.handle(call("skip", json!({})));
        assert!(host.skipped);
        assert!(host.handle(call("update_profile", json!({ "name": "Evil", "tagline": "new line", "interests": ["a", "b"] }))).starts_with("Updated tagline, interests"));
        assert!(host.handle(call("update_profile", json!({ "name": "Evil" }))).starts_with("Failed"));
        let saved: Value = serde_json::from_str(&std::fs::read_to_string(&host.profile).unwrap()).unwrap();
        assert_eq!((saved["tagline"].as_str(), saved["name"].as_str(), saved["interests"].as_array().map(Vec::len)), (Some("new line"), None, Some(2)));
        std::fs::remove_file(&host.profile).ok();
        assert!(host.handle(call("join_call", json!({}))).starts_with("Joined voice channel 5"));
        assert!(host.handle(call("join_call", json!({ "channelId": "77" }))).starts_with("Joined voice channel 77"));
        assert!(host.handle(call("join_call", json!({ "channelId": "nope" }))).starts_with("Failed"));
        assert!(host.handle(call("remind", json!({ "minutes": 10, "note": "Rejoin the call" }))).starts_with("Reminder 1 set"));
        assert!(host.handle(call("remind", json!({ "minutes": 0, "note": "x" }))).starts_with("Failed"));
        assert!(host.handle(call("remind", json!({ "note": "x" }))).starts_with("Failed"));
        assert!(host.handle(call("list_reminders", json!({}))).contains("Rejoin the call"));
        assert_eq!(reminders.list(1).unwrap()[0].voice_channel, Some(77));
        assert_eq!(host.handle(call("cancel_reminder", json!({ "id": 1 }))), "Cancelled reminder 1.");
        assert!(host.handle(call("cancel_reminder", json!({ "id": 1 }))).starts_with("Failed"));
        assert_eq!(host.handle(call("leave_call", json!({}))), "Left the call.");
        assert_eq!(jobs.claim().unwrap().unwrap().task, "fix it\n\nContext:\nrepo is foo");
        assert_eq!((fake.joined.clone(), fake.left), (vec![None, Some(77)], true));
        assert_eq!((fake.spoken, fake.posted), (vec!["hi".to_string()], vec![("10".to_string(), "yo".to_string())]));
    }

    #[test]
    fn voice_tools_tune_live_and_cloning_needs_the_hosts_switch() {
        let _guard = crate::tuning::TEST_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!("wallis-host-voice-{}", std::process::id()));
        std::fs::create_dir_all(root.join("media")).unwrap();
        std::fs::write(root.join("media/johnny.wav"), "RIFF").unwrap();
        let (mut fake, jobs, reminders) = (Fake::default(), Jobs::open(":memory:").unwrap(), Reminders::open(":memory:").unwrap());
        let mut host = Host { discord: &mut fake, jobs: &jobs, reminders: &reminders, guild: 1, user: 2, write_channels: vec![], skipped: false, profile: root.join("profile.json") };
        assert!(host.handle(call("tune", json!({ "speed": 1.1, "temp": 0.6 }))).starts_with("Changed speed, temp."));
        assert!(host.handle(call("tune", json!({ "speed": 9 }))).starts_with("Failed"));
        assert!(host.handle(call("tune", json!({ "nope": 1 }))).starts_with("Failed"));
        assert!(host.handle(call("list_voices", json!({}))).contains("Presets: "));
        assert!(host.handle(call("clone_voice", json!({ "name": "johnny", "path": "johnny.wav" }))).contains("not enabled"));
        assert!(host.handle(call("clone_voice", json!({ "name": "alba", "path": "johnny.wav" }))).contains("built-in"));
        assert!(host.handle(call("channel_history", json!({ "channelId": "10" }))).starts_with("Failed"));
        std::fs::remove_dir_all(root).unwrap();
    }
}
