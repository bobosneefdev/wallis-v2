//! Executes the tool calls the realtime agent makes. Discord itself sits behind `Discord` so this tests without a bot.
use crate::jobs::Jobs;
use crate::pi::HostCall;
use crate::schedule::{self, Reminders};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub trait Discord {
    fn say(&mut self, text: &str) -> Result<(), String>;
    fn message(&mut self, channel_id: &str, text: &str) -> Result<(), String>;
    fn file(&mut self, channel_id: &str, filename: &str, content: &str, text: &str) -> Result<(), String>;
    /// Joins `channel`, or the voice channel of whoever Wallis is talking to. Returns the channel joined.
    fn join(&mut self, _channel: Option<u64>) -> Result<u64, String> { Err("Only the live Wallis can join calls.".into()) }
    /// Leaves the call.
    fn leave(&mut self) -> Result<(), String> { Err("Only the live Wallis can leave calls.".into()) }
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
    /// The returned string is the tool result the model sees, including failures (the model can recover from those).
    pub fn handle(&mut self, call: HostCall) -> String {
        let text = |key: &str| call.args.get(key).and_then(Value::as_str).unwrap_or("").to_string();
        let channel = text("channelId");
        let outcome = match call.tool.as_str() {
            "say" => self.discord.say(&text("text")).map(|_| "Spoken.".to_string()),
            "update_profile" => update_profile(&self.profile, &call.args),
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
                    self.jobs.enqueue(&format!("{task}\n\nContext:\n{context}"), &channel).map(|id| format!("Queued as job {id}. Tell the user it is underway."))
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
    struct Fake { spoken: Vec<String>, posted: Vec<(String, String)>, joined: Vec<Option<u64>>, left: bool, in_call: Option<u64> }
    impl Discord for Fake {
        fn say(&mut self, text: &str) -> Result<(), String> { self.spoken.push(text.into()); Ok(()) }
        fn message(&mut self, channel_id: &str, text: &str) -> Result<(), String> { self.posted.push((channel_id.into(), text.into())); Ok(()) }
        fn file(&mut self, _: &str, _: &str, _: &str, _: &str) -> Result<(), String> { Err("upload failed".into()) }
        fn join(&mut self, channel: Option<u64>) -> Result<u64, String> { self.joined.push(channel); self.in_call = Some(channel.unwrap_or(5)); Ok(channel.unwrap_or(5)) }
        fn leave(&mut self) -> Result<(), String> { self.left = true; self.in_call = None; Ok(()) }
        fn voice_channel(&self) -> Option<u64> { self.in_call }
    }

    fn call(tool: &str, args: Value) -> HostCall { HostCall { request_id: "1".into(), tool: tool.into(), args } }

    #[test]
    fn routes_tools_enforces_write_channels_and_queues_delegations() {
        let (mut fake, jobs, reminders) = (Fake::default(), Jobs::open(":memory:").unwrap(), Reminders::open(":memory:").unwrap());
        let mut host = Host { discord: &mut fake, jobs: &jobs, reminders: &reminders, guild: 1, user: 2, write_channels: vec!["10".into()], skipped: false, profile: std::env::temp_dir().join(format!("wallis-host-profile-{}.json", std::process::id())) };
        assert_eq!(host.handle(call("say", json!({ "text": "hi" }))), "Spoken.");
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
}
