//! Counters and the audit log the brain updates and the Home page reads.
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// How many audit events are kept; older ones fall off.
const AUDIT_LIMIT: usize = 256;

#[derive(Default)]
pub struct Stats { turns: AtomicU64, steers: AtomicU64, total_ms: AtomicU64, last_ms: AtomicU64, errors: AtomicU64, audit: Mutex<VecDeque<Event>> }

/// One audit entry. `kind` is `heard` (voice transcript), `typed` (text message), `prompt`, `steer` (sent to the realtime agent) or `delegate`.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Event { pub at_ms: u64, pub kind: &'static str, pub text: String }

#[derive(Debug, Default, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot { pub turns: u64, pub steers: u64, pub errors: u64, pub last_turn_ms: u64, pub average_turn_ms: u64 }

impl Stats {
    pub fn turn(&self, ms: u64, ok: bool) {
        self.turns.fetch_add(1, Relaxed);
        self.total_ms.fetch_add(ms, Relaxed);
        self.last_ms.store(ms, Relaxed);
        if !ok { self.errors.fetch_add(1, Relaxed); }
    }
    pub fn steered(&self) { self.steers.fetch_add(1, Relaxed); }
    pub fn audit(&self, kind: &'static str, text: &str) {
        let at_ms = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
        let mut audit = self.audit.lock().expect("audit poisoned");
        if audit.len() == AUDIT_LIMIT { audit.pop_front(); }
        audit.push_back(Event { at_ms, kind, text: text.into() });
    }
    /// Oldest first.
    pub fn audit_log(&self) -> Vec<Event> { self.audit.lock().expect("audit poisoned").iter().cloned().collect() }
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
    fn audit_keeps_only_the_newest_events() {
        let stats = Stats::default();
        for i in 0..AUDIT_LIMIT + 2 { stats.audit("heard", &i.to_string()); }
        let log = stats.audit_log();
        assert_eq!((log.len(), log[0].text.as_str(), log[AUDIT_LIMIT - 1].text.as_str()), (AUDIT_LIMIT, "2", "257"));
    }
}
