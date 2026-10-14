//! Executes the tool calls the realtime agent makes. Discord itself sits behind `Discord` so this tests without a bot.
use crate::jobs::Jobs;
use crate::pi::HostCall;
use serde_json::Value;

pub trait Discord {
    fn say(&mut self, text: &str) -> Result<(), String>;
    fn message(&mut self, channel_id: &str, text: &str) -> Result<(), String>;
    fn file(&mut self, channel_id: &str, filename: &str, content: &str, text: &str) -> Result<(), String>;
}

pub struct Host<'a, D: Discord> {
    pub discord: &'a mut D,
    pub jobs: &'a Jobs,
    /// Channels the user allowed Wallis to write to. Anything else is refused.
    pub write_channels: Vec<String>,
    /// Set when the agent chose to stay silent this turn.
    pub skipped: bool,
}

impl<D: Discord> Host<'_, D> {
    /// The returned string is the tool result the model sees, including failures (the model can recover from those).
    pub fn handle(&mut self, call: HostCall) -> String {
        let text = |key: &str| call.args.get(key).and_then(Value::as_str).unwrap_or("").to_string();
        let channel = text("channelId");
        let outcome = match call.tool.as_str() {
            "say" => self.discord.say(&text("text")).map(|_| "Spoken.".to_string()),
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
    struct Fake { spoken: Vec<String>, posted: Vec<(String, String)> }
    impl Discord for Fake {
        fn say(&mut self, text: &str) -> Result<(), String> { self.spoken.push(text.into()); Ok(()) }
        fn message(&mut self, channel_id: &str, text: &str) -> Result<(), String> { self.posted.push((channel_id.into(), text.into())); Ok(()) }
        fn file(&mut self, _: &str, _: &str, _: &str, _: &str) -> Result<(), String> { Err("upload failed".into()) }
    }

    fn call(tool: &str, args: Value) -> HostCall { HostCall { request_id: "1".into(), tool: tool.into(), args } }

    #[test]
    fn routes_tools_enforces_write_channels_and_queues_delegations() {
        let (mut fake, jobs) = (Fake::default(), Jobs::open(":memory:").unwrap());
        let mut host = Host { discord: &mut fake, jobs: &jobs, write_channels: vec!["10".into()], skipped: false };
        assert_eq!(host.handle(call("say", json!({ "text": "hi" }))), "Spoken.");
        assert_eq!(host.handle(call("message", json!({ "channelId": "10", "text": "yo" }))), "Posted.");
        assert!(host.handle(call("message", json!({ "channelId": "99", "text": "no" }))).starts_with("Failed: Channel"));
        assert!(host.handle(call("file", json!({ "channelId": "10", "filename": "a.rs", "content": "x" }))).contains("upload failed"));
        assert!(host.handle(call("delegate", json!({ "task": "fix it", "context": "repo is foo", "channelId": "10" }))).starts_with("Queued as job"));
        assert!(host.handle(call("delegate", json!({ "task": "x", "context": "", "channelId": "99" }))).starts_with("Failed"));
        assert!(host.handle(call("nope", json!({}))).contains("Unknown tool"));
        host.handle(call("skip", json!({})));
        assert!(host.skipped);
        assert_eq!(jobs.claim().unwrap().unwrap().task, "fix it\n\nContext:\nrepo is foo");
        assert_eq!((fake.spoken, fake.posted), (vec!["hi".to_string()], vec![("10".to_string(), "yo".to_string())]));
    }
}
