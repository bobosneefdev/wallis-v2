//! Turn counters for the Home page, and the live log: one process-wide ring of timestamped lines that every part of the
//! runtime writes to (and mirrors to stderr, which the desktop keeps as `runtime.log`).
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// How many log lines are kept in memory; older ones fall off (`runtime.log` keeps everything since start).
const LOG_LIMIT: usize = 2000;
/// Longer lines (full prompts, job reports) are cut so one line cannot flood the view.
const LINE_LIMIT: usize = 4000;

static LOG: Mutex<VecDeque<Line>> = Mutex::new(VecDeque::new());
static SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Default)]
pub struct Stats { turns: AtomicU64, steers: AtomicU64, total_ms: AtomicU64, last_ms: AtomicU64, errors: AtomicU64 }

/// One log line. `seq` increases by one per line, so a reader asks only for what is new.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Line { pub seq: u64, pub at_ms: u64, pub tag: &'static str, pub text: String }

/// Log lines after `since`, and the `seq` to ask from next time. A `next` below what the reader already has means the runtime restarted.
#[derive(Debug, serde::Serialize)]
pub struct Tail { pub next: u64, pub lines: Vec<Line> }

#[derive(Debug, Default, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot { pub turns: u64, pub steers: u64, pub errors: u64, pub last_turn_ms: u64, pub average_turn_ms: u64 }

/// Adds a line to the live log. `tag` is a short lowercase kind (`heard`, `turn`, `model`, `tool`, `tts`, `error`, ...).
pub fn log(tag: &'static str, text: impl AsRef<str>) {
    let text = text.as_ref();
    let text = match text.char_indices().nth(LINE_LIMIT) { Some((cut, _)) => format!("{}…", &text[..cut]), None => text.to_string() };
    let at_ms = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
    eprintln!("{} {tag:<7} {text}", chrono::Local::now().format("%H:%M:%S%.3f"));
    let mut log = LOG.lock().expect("log poisoned");
    if log.len() == LOG_LIMIT { log.pop_front(); }
    log.push_back(Line { seq: SEQ.fetch_add(1, Relaxed) + 1, at_ms, tag, text });
}

/// Oldest first.
pub fn tail(since: u64) -> Tail {
    let log = LOG.lock().expect("log poisoned");
    Tail { next: SEQ.load(Relaxed), lines: log.iter().filter(|line| line.seq > since).cloned().collect() }
}

/// Seconds with one decimal, for log lines.
pub fn secs(duration: std::time::Duration) -> String { format!("{:.1} s", duration.as_secs_f64()) }

impl Stats {
    pub fn turn(&self, ms: u64, ok: bool) {
        self.turns.fetch_add(1, Relaxed);
        self.total_ms.fetch_add(ms, Relaxed);
        self.last_ms.store(ms, Relaxed);
        if !ok { self.errors.fetch_add(1, Relaxed); }
    }
    pub fn steered(&self) { self.steers.fetch_add(1, Relaxed); }
    pub fn snapshot(&self) -> Snapshot {
        let turns = self.turns.load(Relaxed);
        Snapshot { turns, steers: self.steers.load(Relaxed), errors: self.errors.load(Relaxed), last_turn_ms: self.last_ms.load(Relaxed), average_turn_ms: self.total_ms.load(Relaxed).checked_div(turns).unwrap_or(0) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn averages_turn_times_and_counts_failures() {
        let stats = Stats::default();
        stats.turn(100, true);
        stats.turn(300, false);
        stats.steered();
        assert_eq!(stats.snapshot(), Snapshot { turns: 2, steers: 1, errors: 1, last_turn_ms: 300, average_turn_ms: 200 });
    }

    #[test]
    fn the_log_keeps_the_newest_lines_and_serves_only_what_is_new() {
        let start = tail(u64::MAX).next;
        for i in 0..LOG_LIMIT + 2 { log("test", i.to_string()); }
        log("test", "x".repeat(LINE_LIMIT + 10));
        // Other tests log too, so look for this test's lines rather than assuming they are the only ones.
        let all = tail(0);
        assert_eq!(all.lines.len(), LOG_LIMIT);
        assert!(all.next >= start + LOG_LIMIT as u64 + 3);
        assert!(all.lines.windows(2).all(|pair| pair[1].seq > pair[0].seq));
        let long = all.lines.iter().rev().find(|line| line.tag == "test").unwrap();
        assert_eq!(long.text.chars().count(), LINE_LIMIT + 1, "long lines are cut");
        assert!(tail(long.seq).lines.iter().all(|line| line.seq > long.seq), "only newer lines are served");
    }
}
