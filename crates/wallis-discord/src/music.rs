//! Music uses independent Songbird tracks; speech interruption only touches the speech queue.
use songbird::events::{Event, EventContext, EventHandler};
use songbird::tracks::TrackHandle;
use songbird::events::TrackEvent;
use songbird::Songbird;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use twilight_model::id::{marker::GuildMarker, Id};

#[derive(Default)]
struct Guild { queue: VecDeque<(PathBuf, String)>, current: Option<(TrackHandle, String)>, volume: Option<u32>, speech: usize, generation: u64 }
pub(crate) struct Player { voice: Arc<Songbird>, guilds: Mutex<HashMap<u64, Guild>> }
impl Player {
    pub fn new(voice: Arc<Songbird>) -> Arc<Self> { Arc::new(Self { voice, guilds: Mutex::default() }) }
    async fn next(self: &Arc<Self>, guild: u64, state: &mut Guild) -> Result<(), String> {
        if state.current.is_some() { return Ok(()); }
        let Some((path, title)) = state.queue.pop_front() else { return Ok(()) };
        let result = async {
            let call = self.voice.get(Id::<GuildMarker>::new(guild)).ok_or("Wallis is not in a call.")?;
            let track = call.lock().await.play_input(songbird::input::File::new(path.clone()).into());
            track.set_volume(level(state)).map_err(|e| e.to_string())?;
            track.add_event(Event::Track(TrackEvent::End), End { player: self.clone(), guild, path: path.clone() }).map_err(|e| e.to_string())?;
            state.current = Some((track, title));
            Ok::<_, String>(())
        }.await;
        if result.is_err() { let _ = std::fs::remove_file(path); }
        result
    }
    pub async fn queue(self: &Arc<Self>, guild: u64, path: PathBuf, title: String) -> Result<(), String> {
        let mut guilds = self.guilds.lock().await;
        let state = guilds.entry(guild).or_default();
        if state.queue.len() >= 25 { return Err("Music queue is full (25 waiting tracks).".into()); }
        state.queue.push_back((path, title));
        self.next(guild, state).await
    }
    pub async fn list(&self, guild: u64) -> String {
        let guilds = self.guilds.lock().await;
        let Some(state) = guilds.get(&guild) else { return "The queue is empty.".into() };
        let mut lines = state.current.as_ref().map(|(_,t)| format!("Playing: {t}")).into_iter().collect::<Vec<_>>();
        lines.extend(state.queue.iter().enumerate().map(|(i,(_,t))| format!("{}. {t}", i+1)));
        if lines.is_empty() { "The queue is empty.".into() } else { lines.join("\n") }
    }
    pub async fn skip(&self, guild: u64) -> Result<(), String> {
        let guilds = self.guilds.lock().await;
        if let Some((track,_)) = guilds.get(&guild).and_then(|s| s.current.as_ref()) { track.stop().map_err(|e| e.to_string())?; }
        Ok(())
    }
    pub async fn stop(&self, guild: u64) -> Result<(), String> {
        let mut guilds = self.guilds.lock().await;
        if let Some(state) = guilds.get_mut(&guild) {
            for (path,_) in state.queue.drain(..) { let _ = std::fs::remove_file(path); }
            if let Some((track,_)) = &state.current { track.stop().map_err(|e| e.to_string())?; }
        }
        Ok(())
    }
    pub async fn volume(&self, guild: u64, percent: u32) -> Result<(), String> {
        if percent > 100 { return Err("Volume must be 0–100.".into()); }
        let mut guilds = self.guilds.lock().await;
        let state = guilds.entry(guild).or_default(); state.volume = Some(percent);
        if let Some((track,_)) = &state.current { track.set_volume(level(state)).map_err(|e| e.to_string())?; }
        Ok(())
    }
    pub async fn speaking(self: &Arc<Self>, guild: u64) -> SpeechEnd {
        let mut guilds = self.guilds.lock().await;
        let state = guilds.entry(guild).or_default(); state.speech += 1;
        if let Some((track,_)) = &state.current { let _ = track.set_volume(level(state)); }
        SpeechEnd { player: self.clone(), guild, generation: state.generation }
    }
    pub async fn pending_speech(&self, guild: u64) -> usize { self.guilds.lock().await.get(&guild).map_or(0, |s| s.speech) }
    pub async fn reset_speech(&self, guild: u64) {
        let mut guilds = self.guilds.lock().await;
        if let Some(state) = guilds.get_mut(&guild) {
            state.generation += 1; state.speech = 0;
            if let Some((track,_)) = &state.current { let _ = track.set_volume(level(state)); }
        }
    }
}
fn level(state: &Guild) -> f32 { state.volume.unwrap_or(70) as f32 / 100.0 * if state.speech > 0 { 0.25 } else { 1.0 } }
struct End { player: Arc<Player>, guild: u64, path: PathBuf }
#[async_trait::async_trait]
impl EventHandler for End {
    async fn act(&self, _: &EventContext<'_>) -> Option<Event> {
        let _ = std::fs::remove_file(&self.path);
        let mut guilds = self.player.guilds.lock().await;
        if let Some(state) = guilds.get_mut(&self.guild) {
            state.current = None;
            if let Err(error) = self.player.next(self.guild, state).await { eprintln!("wallis: music: {error}"); }
        }
        None
    }
}
pub(crate) struct SpeechEnd { player: Arc<Player>, guild: u64, generation: u64 }
#[async_trait::async_trait]
impl EventHandler for SpeechEnd {
    async fn act(&self, _: &EventContext<'_>) -> Option<Event> {
        let mut guilds = self.player.guilds.lock().await;
        if let Some(state) = guilds.get_mut(&self.guild).filter(|s| s.generation == self.generation) {
            state.speech = state.speech.saturating_sub(1);
            if let Some((track,_)) = &state.current { let _ = track.set_volume(level(state)); }
        }
        None
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn volume_ducks_only_while_speech_is_pending() {
        let mut state = Guild::default(); state.volume = Some(40);
        assert_eq!(level(&state), 0.4);
        state.speech = 2; assert_eq!(level(&state), 0.1);
        state.speech = 0; assert_eq!(level(&state), 0.4);
        state.volume = Some(0); assert_eq!(level(&state), 0.0);
    }
}
