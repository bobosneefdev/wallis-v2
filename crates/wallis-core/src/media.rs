//! Discord media metadata stays in RAM; only selected media is fetched, outside the gateway loop.
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const SCRIPT: &str = include_str!("../py/media.py");
pub const MAX_SOURCES: usize = 4;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Image { pub r#type: String, pub data: String, pub mime_type: String }

#[derive(Deserialize)]
pub struct Prepared { pub images: Vec<Image>, pub note: String }

/// The process deadline also bounds DNS and native decoding. Output and downloads are independently capped.
pub fn prepare(python: &str, source: &str, limit: usize, timeout: Duration) -> Result<Prepared, String> {
    let mut command = Command::new(python);
    command.args(["-I", "-c", SCRIPT]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    #[cfg(windows)]
    { use std::os::windows::process::CommandExt; command.creation_flags(0x0800_0000); }
    let mut child = command.spawn().map_err(|_| "The media decoder could not start.".to_string())?;
    let input = serde_json::json!({ "url": source, "limit": limit }).to_string();
    let write = child.stdin.take().unwrap().write_all(input.as_bytes());
    if write.is_err() { let _ = child.kill(); let _ = child.wait(); return Err("The media decoder rejected its input.".into()); }
    let stdout = child.stdout.take().unwrap();
    let (send, receive) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stdout.take(8 * 1024 * 1024 + 1).read_to_end(&mut bytes).map(|_| bytes);
        let _ = send.send(result);
    });
    let result = receive.recv_timeout(timeout);
    let _ = child.kill();
    let _ = child.wait();
    let bytes = result.map_err(|_| "Media processing exceeded the 20 second limit.")?.map_err(|_| "Could not read decoded media.")?;
    if bytes.len() > 8 * 1024 * 1024 { return Err("Decoded media exceeded the output limit.".into()); }
    serde_json::from_slice(&bytes).map_err(|_| "The media decoder returned invalid output.".into())
}

struct Entry { channel: u64, message: u64, user: u64, at: Instant, sources: Vec<String> }

#[derive(Default)]
pub struct History(VecDeque<Entry>);

impl History {
    pub fn forget(&mut self, channel: u64, messages: &[u64]) {
        self.0.retain(|entry| entry.channel != channel || !messages.contains(&entry.message));
    }

    pub fn remember(&mut self, channel: u64, message: u64, user: u64, sources: &[String]) {
        let now = Instant::now();
        self.0.retain(|entry| now.duration_since(entry.at) < Duration::from_secs(600) && !(entry.channel == channel && entry.message == message));
        if sources.is_empty() { return; }
        self.0.push_back(Entry { channel, message, user, at: now, sources: sources.iter().take(MAX_SOURCES).cloned().collect() });
        while self.0.len() > 64 { self.0.pop_front(); }
    }

    pub fn resolve(&self, channel: u64, user: u64, reply: Option<u64>, said: &str) -> Option<(u64, Vec<String>)> {
        let recent = said.to_lowercase();
        let refers = ["above", "that image", "that gif", "that video", "this image", "this gif", "this video", "picture", "photo", "look at", "what does it show", "see this", "see it", "see that", "what is this", "what's this", "describe it", "describe this"].iter().any(|phrase| recent.contains(phrase));
        self.0.iter().rev().find(|entry| entry.channel == channel && entry.at.elapsed() < Duration::from_secs(600) &&
            match reply { Some(id) => entry.message == id, None => refers && entry.user == user })
            .map(|entry| (entry.message, entry.sources.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn references_prefer_replies_and_stay_within_the_channel_and_user() {
        let mut history = History::default();
        let source = "https://cdn.discordapp.com/test.png".to_string();
        history.remember(1, 10, 2, std::slice::from_ref(&source));
        history.remember(1, 11, 3, std::slice::from_ref(&source));
        assert_eq!(history.resolve(1, 2, None, "Wallis what does the gif above show?"), Some((10, vec![source.clone()])));
        assert_eq!(history.resolve(1, 2, Some(11), "what is this?"), Some((11, vec![source])));
        assert!(history.resolve(2, 2, Some(10), "above").is_none());
        assert!(history.resolve(1, 4, None, "above").is_none());
        assert!(history.resolve(1, 2, None, "how are you?").is_none());
        assert!(history.resolve(1, 2, Some(99), "above").is_none());
        history.0[0].at = Instant::now() - Duration::from_secs(601);
        assert!(history.resolve(1, 2, None, "above").is_none());
        for id in 0..100 { history.remember(1, id, 2, &["https://example.com/a.png".to_string()]); }
        assert_eq!(history.0.len(), 64);
        history.forget(1, &[99]);
        assert!(history.resolve(1, 2, Some(99), "above").is_none());
    }
}
