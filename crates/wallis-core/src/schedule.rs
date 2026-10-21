//! Wallis' sense of time: reminders he sets for himself, kept in SQLite so they survive restarts.
//! A reminder is only a note to his future self; what he does when it fires is up to him, so the same
//! mechanism covers "be right back", "check on the build in an hour" and "remind Jake about the dentist".
use chrono::{Local, TimeZone};
use rusqlite::{params, Connection};
use std::time::{SystemTime, UNIX_EPOCH};

pub const MAX_MINUTES: f64 = 7.0 * 24.0 * 60.0;
pub const MAX_PENDING: i64 = 20;

pub fn now() -> i64 { SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0) }

/// Local wall-clock time as the model reads it, e.g. `Tue Oct 14, 14:32`.
pub fn stamp(unix: i64) -> String {
    Local.timestamp_opt(unix, 0).single().map(|time| time.format("%a %b %-d, %H:%M").to_string()).unwrap_or_else(|| unix.to_string())
}

/// A duration in words, rounded to the minute.
pub fn span(secs: i64) -> String {
    let minutes = (secs.max(0) + 30) / 60;
    let unit = |n: i64, name: &str| format!("{n} {name}{}", if n == 1 { "" } else { "s" });
    match minutes {
        0 => "under a minute".into(),
        1..=59 => unit(minutes, "minute"),
        60..=2879 => if minutes % 60 == 0 { unit(minutes / 60, "hour") } else { format!("{} {}", unit(minutes / 60, "hour"), unit(minutes % 60, "minute")) },
        _ => unit((minutes + 720) / 1440, "day"),
    }
}

#[derive(Clone, Debug, PartialEq)]
/// `origin`: the text channel it was set in, or 0 when set in the voice call.
pub struct Reminder { pub id: i64, pub due_at: i64, pub created_at: i64, pub note: String, pub guild: u64, pub user: u64, pub voice_channel: Option<u64>, pub origin: u64 }

impl Reminder {
    /// The turn prompt Wallis gets when this fires. `in_call` is the voice channel he is in right now.
    pub fn prompt(&self, now: i64, in_call: Option<u64>, write_channels: &[String]) -> String {
        let late = if now - self.due_at > 120 { format!(" It was due {} ago, so you are running late.", span(now - self.due_at)) } else { String::new() };
        let then = self.voice_channel.map(|id| format!(" You were in voice channel {id} when you set it.")).unwrap_or_default();
        let here = in_call.map(|id| format!(" You are in voice channel {id} right now.")).unwrap_or_else(|| " You are not in a voice call right now.".into());
        let channels = if write_channels.is_empty() { String::new() } else { format!(" Channels you may post in: {}.", write_channels.join(", ")) };
        let origin = if self.origin != 0 { format!(" You set it while chatting in text channel {}, where they may not be in your call: act there with `message` (written to stand on its own) rather than aloud, unless the note says to speak or join.", self.origin) } else { String::new() };
        format!("[reminder] At {} you told yourself: \"{}\".{late}{then}{here}{origin}{channels} Act on it naturally, as yourself.", stamp(self.created_at), self.note)
    }
}

pub struct Reminders(Connection);

fn row(row: &rusqlite::Row) -> rusqlite::Result<Reminder> {
    Ok(Reminder { id: row.get(0)?, due_at: row.get(1)?, created_at: row.get(2)?, note: row.get(3)?, guild: row.get::<_, i64>(4)? as u64, user: row.get::<_, i64>(5)? as u64, voice_channel: row.get::<_, Option<i64>>(6)?.map(|id| id as u64), origin: row.get::<_, i64>(7)? as u64 })
}

const COLUMNS: &str = "id, due_at, created_at, note, guild, user, voice_channel, origin";

impl Reminders {
    pub fn open(path: &str) -> Result<Self, String> {
        let db = Connection::open(path).map_err(|error| format!("Unable to open the reminder store: {error}"))?;
        db.execute_batch("CREATE TABLE IF NOT EXISTS reminders (id INTEGER PRIMARY KEY, due_at INTEGER NOT NULL, created_at INTEGER NOT NULL, note TEXT NOT NULL, guild INTEGER NOT NULL, user INTEGER NOT NULL, voice_channel INTEGER)")
            .map_err(|error| format!("Unable to prepare the reminder store: {error}"))?;
        let migrated: i64 = db.query_row("SELECT COUNT(*) FROM pragma_table_info('reminders') WHERE name='origin'", [], |r| r.get(0)).map_err(|error| error.to_string())?;
        if migrated == 0 { db.execute("ALTER TABLE reminders ADD COLUMN origin INTEGER NOT NULL DEFAULT 0", []).map_err(|error| error.to_string())?; }
        Ok(Self(db))
    }

    /// Schedules a reminder `minutes` from `now`. The bounds are checked here because the model supplies them.
    #[allow(clippy::too_many_arguments)]
    pub fn add(&self, now: i64, minutes: f64, note: &str, guild: u64, user: u64, voice_channel: Option<u64>, origin: u64) -> Result<Reminder, String> {
        if !minutes.is_finite() || !(1.0..=MAX_MINUTES).contains(&minutes) { return Err("`minutes` must be between 1 and 10080 (one week).".into()); }
        let note = note.trim();
        if note.is_empty() { return Err("`note` is required: say what you are doing and what to do when it fires.".into()); }
        let note: String = note.chars().take(1000).collect();
        let pending: i64 = self.0.query_row("SELECT COUNT(*) FROM reminders WHERE guild=?1", params![guild as i64], |r| r.get(0)).map_err(|error| error.to_string())?;
        if pending >= MAX_PENDING { return Err(format!("You already have {MAX_PENDING} reminders pending; cancel one first.")); }
        let due_at = now + (minutes * 60.0).round() as i64;
        self.0.execute("INSERT INTO reminders (due_at, created_at, note, guild, user, voice_channel, origin) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)", params![due_at, now, note, guild as i64, user as i64, voice_channel.map(|id| id as i64), origin as i64]).map_err(|error| error.to_string())?;
        Ok(Reminder { id: self.0.last_insert_rowid(), due_at, created_at: now, note, guild, user, voice_channel, origin })
    }

    pub fn cancel(&self, id: i64, guild: u64) -> Result<bool, String> {
        self.0.execute("DELETE FROM reminders WHERE id=?1 AND guild=?2", params![id, guild as i64]).map(|n| n == 1).map_err(|error| error.to_string())
    }

    pub fn list(&self, guild: u64) -> Result<Vec<Reminder>, String> {
        let mut statement = self.0.prepare(&format!("SELECT {COLUMNS} FROM reminders WHERE guild=?1 ORDER BY due_at, id")).map_err(|error| error.to_string())?;
        let rows = statement.query_map(params![guild as i64], row).map_err(|error| error.to_string())?;
        rows.collect::<Result<_, _>>().map_err(|error| error.to_string())
    }

    /// Removes and returns everything due, oldest first. Removal is the claim: a reminder fires at most once, even across a crash.
    pub fn take_due(&self, now: i64) -> Result<Vec<Reminder>, String> {
        let mut statement = self.0.prepare(&format!("DELETE FROM reminders WHERE due_at <= ?1 RETURNING {COLUMNS}")).map_err(|error| error.to_string())?;
        let mut due = statement.query_map(params![now], row).map_err(|error| error.to_string())?.collect::<Result<Vec<_>, _>>().map_err(|error| error.to_string())?;
        due.sort_by_key(|reminder| (reminder.due_at, reminder.id));
        Ok(due)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reminders_fire_once_in_order_and_survive_until_due() {
        let store = Reminders::open(":memory:").unwrap();
        let late = store.add(1000, 20.0, "later", 1, 2, None, 0).unwrap();
        let soon = store.add(1000, 10.0, "groceries", 1, 2, Some(7), 0).unwrap();
        assert_eq!((soon.due_at, late.due_at), (1600, 2200));
        assert!(store.take_due(1599).unwrap().is_empty());
        assert_eq!(store.take_due(5000).unwrap().iter().map(|r| r.note.as_str()).collect::<Vec<_>>(), ["groceries", "later"]);
        assert!(store.take_due(5000).unwrap().is_empty());
    }

    #[test]
    fn validates_what_the_model_supplies() {
        let store = Reminders::open(":memory:").unwrap();
        for minutes in [0.0, 0.5, -3.0, f64::NAN, f64::INFINITY, MAX_MINUTES + 1.0] { assert!(store.add(0, minutes, "x", 1, 1, None, 0).is_err(), "{minutes}"); }
        assert!(store.add(0, 5.0, "   ", 1, 1, None, 0).is_err());
        for _ in 0..MAX_PENDING { store.add(0, 5.0, "x", 1, 1, None, 0).unwrap(); }
        assert!(store.add(0, 5.0, "x", 1, 1, None, 0).unwrap_err().contains("cancel one"));
        assert!(store.add(0, 5.0, "x", 2, 1, None, 0).is_ok());
    }

    #[test]
    fn cancel_and_list_are_scoped_to_the_guild() {
        let store = Reminders::open(":memory:").unwrap();
        let mine = store.add(0, 5.0, "mine", 1, 1, None, 0).unwrap();
        assert!(!store.cancel(mine.id, 2).unwrap());
        assert_eq!(store.list(1).unwrap(), [mine.clone()]);
        assert!(store.list(2).unwrap().is_empty());
        assert!(store.cancel(mine.id, 1).unwrap() && store.list(1).unwrap().is_empty());
    }

    #[test]
    fn durations_read_naturally() {
        assert_eq!([0, 59, 600, 3600, 3900, 7200 + 60, 86_400 * 3].map(span), ["under a minute", "1 minute", "10 minutes", "1 hour", "1 hour 5 minutes", "2 hours 1 minute", "3 days"]);
    }

    #[test]
    fn the_fire_prompt_carries_the_note_and_where_he_is() {
        let reminder = Reminder { id: 1, due_at: 0, created_at: 0, note: "Rejoin the call".into(), guild: 1, user: 2, voice_channel: Some(7), origin: 0 };
        let prompt = reminder.prompt(60, None, &["10".into()]);
        assert!(prompt.starts_with("[reminder]") && prompt.contains("Rejoin the call") && prompt.contains("voice channel 7 when") && prompt.contains("not in a voice call") && prompt.contains("10") && !prompt.contains("late"));
        assert!(reminder.prompt(900, Some(7), &[]).contains("running late") && reminder.prompt(900, Some(7), &[]).contains("in voice channel 7 right now"));
    }
}
