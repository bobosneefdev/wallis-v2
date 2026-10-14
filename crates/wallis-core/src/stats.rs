//! Counters the brain updates and the Home page reads.
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

#[derive(Default)]
pub struct Stats { turns: AtomicU64, steers: AtomicU64, total_ms: AtomicU64, last_ms: AtomicU64, errors: AtomicU64 }

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
}
