//! Durable background jobs. Interrupted jobs are never silently re-run: they are marked failed so the
//! user can retry (a re-run could post twice).
use rusqlite::{params, Connection};

pub struct Jobs(Connection);

#[derive(Debug, PartialEq)]
pub struct Job { pub id: i64, pub task: String, pub channel_id: String, pub status: String }

impl Jobs {
    pub fn open(path: &str) -> Result<Self, String> {
        let db = Connection::open(path).map_err(|error| format!("Unable to open the job store: {error}"))?;
        db.execute_batch("CREATE TABLE IF NOT EXISTS jobs (id INTEGER PRIMARY KEY, task TEXT NOT NULL, channel_id TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'queued', result TEXT, created_at INTEGER NOT NULL DEFAULT (unixepoch()))")
            .map_err(|error| format!("Unable to prepare the job store: {error}"))?;
        Ok(Self(db))
    }

    /// Call once at startup: anything still queued/running was cut off by the restart.
    pub fn fail_interrupted(&self) -> Result<Vec<Job>, String> {
        let interrupted = self.with_status(&["queued", "running"])?;
        self.0.execute("UPDATE jobs SET status='failed', result='Interrupted by a restart' WHERE status IN ('queued','running')", []).map_err(|error| error.to_string())?;
        Ok(interrupted)
    }

    pub fn enqueue(&self, task: &str, channel_id: &str) -> Result<i64, String> {
        self.0.execute("INSERT INTO jobs (task, channel_id) VALUES (?1, ?2)", params![task, channel_id]).map_err(|error| error.to_string())?;
        Ok(self.0.last_insert_rowid())
    }

    pub fn finish(&self, id: i64, ok: bool, result: &str) -> Result<(), String> {
        self.0.execute("UPDATE jobs SET status=?2, result=?3 WHERE id=?1", params![id, if ok { "done" } else { "failed" }, result]).map(|_| ()).map_err(|error| error.to_string())
    }

    pub fn with_status(&self, statuses: &[&str]) -> Result<Vec<Job>, String> {
        let mut statement = self.0.prepare("SELECT id, task, channel_id, status FROM jobs ORDER BY id").map_err(|error| error.to_string())?;
        let rows = statement.query_map([], |row| Ok(Job { id: row.get(0)?, task: row.get(1)?, channel_id: row.get(2)?, status: row.get(3)? })).map_err(|error| error.to_string())?;
        Ok(rows.filter_map(Result::ok).filter(|job| statuses.contains(&job.status.as_str())).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restart_marks_unfinished_jobs_failed_instead_of_rerunning_them() {
        let jobs = Jobs::open(":memory:").unwrap();
        let (a, b, c) = (jobs.enqueue("one", "1").unwrap(), jobs.enqueue("two", "1").unwrap(), jobs.enqueue("three", "2").unwrap());
        jobs.finish(a, true, "ok").unwrap();
        let interrupted = jobs.fail_interrupted().unwrap();
        assert_eq!(interrupted.iter().map(|job| job.id).collect::<Vec<_>>(), vec![b, c]);
        assert_eq!(jobs.with_status(&["failed"]).unwrap().len(), 2);
        assert!(jobs.fail_interrupted().unwrap().is_empty(), "idempotent");
    }
}
