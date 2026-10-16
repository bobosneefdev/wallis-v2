//! Durable background jobs. Interrupted jobs are never silently re-run: they are marked failed so the
//! user can retry (a re-run could post twice).
use rusqlite::{params, Connection};

pub struct Jobs(Connection);

#[derive(Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Job { pub id: i64, pub task: String, pub channel_id: String, pub status: String, pub guild: u64, pub user: u64, pub result: Option<String> }

impl Job {
    /// The private turn Wallis gets when his worker finishes: the user never saw the worker, so he is the one who tells them.
    pub fn prompt(&self, now: i64, in_call: Option<u64>, write_channels: &[String]) -> String {
        let task: String = self.task.chars().take(1500).collect();
        let result: String = self.result.as_deref().unwrap_or("").chars().take(12_000).collect();
        let here = in_call.map(|id| format!("You are in voice channel {id} right now: tell them aloud.")).unwrap_or_else(|| format!("You are not in a call: tell them in a text channel (requested in {}; allowed: {}).", self.channel_id, write_channels.join(", ")));
        let outcome = if self.status == "done" { "finished" } else { "FAILED" };
        format!("[job finished] [{}] Your background worker {outcome} job {} (private: the user cannot see this and does not know a worker exists). You asked it: \"{task}\"\n\n--- its report ---\n{result}\n--- end ---\n{here} Say it as yourself, in your own words: if it is something to tell or show (jokes, findings), lead with a quick hook or a offer (\"got those jokes, want them?\") unless they are clearly waiting for it. Never mention workers, jobs or reports; if it failed, say you couldn't manage it and offer to try differently. Use `delegate` again if more work is needed.", crate::schedule::stamp(now), self.id)
    }
}

fn job(row: &rusqlite::Row) -> rusqlite::Result<Job> {
    Ok(Job { id: row.get(0)?, task: row.get(1)?, channel_id: row.get(2)?, status: row.get(3)?, guild: row.get::<_, String>(4)?.parse().unwrap_or(0), user: row.get::<_, String>(5)?.parse().unwrap_or(0), result: row.get(6)? })
}

impl Jobs {
    pub fn open(path: &str) -> Result<Self, String> {
        let db = Connection::open(path).map_err(|error| format!("Unable to open the job store: {error}"))?;
        db.execute_batch("CREATE TABLE IF NOT EXISTS jobs (id INTEGER PRIMARY KEY, task TEXT NOT NULL, channel_id TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'queued', result TEXT, created_at INTEGER NOT NULL DEFAULT (unixepoch()))")
            .map_err(|error| format!("Unable to prepare the job store: {error}"))?;
        db.busy_timeout(std::time::Duration::from_secs(5)).map_err(|e| e.to_string())?;
        for name in ["guild", "user"] {
            let exists: bool = db.prepare("PRAGMA table_info(jobs)").map_err(|e| e.to_string())?.query_map([], |row| row.get::<_, String>(1)).map_err(|e| e.to_string())?.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?.iter().any(|col| col == name);
            if !exists { db.execute(&format!("ALTER TABLE jobs ADD COLUMN {name} TEXT NOT NULL DEFAULT '0'"), []).map_err(|e| e.to_string())?; }
        }
        // `delivered`: the finished job has been handed back to Wallis. Jobs from before this column existed count as delivered.
        let has_delivered = db.prepare("PRAGMA table_info(jobs)").map_err(|e| e.to_string())?.query_map([], |row| row.get::<_, String>(1)).map_err(|e| e.to_string())?.filter_map(Result::ok).any(|col| col == "delivered");
        if !has_delivered { db.execute_batch("ALTER TABLE jobs ADD COLUMN delivered INTEGER NOT NULL DEFAULT 0; UPDATE jobs SET delivered=1").map_err(|e| e.to_string())?; }
        Ok(Self(db))
    }

    /// Finished (done or failed) jobs Wallis has not heard about yet, oldest first. Returning them is the claim: each is handed over once.
    pub fn take_finished(&self) -> Result<Vec<Job>, String> {
        let mut statement = self.0.prepare("UPDATE jobs SET delivered=1 WHERE delivered=0 AND status IN ('done','failed') RETURNING id, task, channel_id, status, guild, user, result").map_err(|e| e.to_string())?;
        let mut jobs = statement.query_map([], job).map_err(|e| e.to_string())?.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?;
        jobs.sort_by_key(|job| job.id);
        Ok(jobs)
    }

    /// Call once at startup: anything still queued/running was cut off by the restart.
    pub fn fail_interrupted(&self) -> Result<Vec<Job>, String> {
        let interrupted = self.with_status(&["queued", "running"])?;
        self.0.execute("UPDATE jobs SET status='failed', result='Interrupted by a restart' WHERE status IN ('queued','running')", []).map_err(|error| error.to_string())?;
        Ok(interrupted)
    }

    pub fn enqueue(&self, task: &str, channel_id: &str) -> Result<i64, String> {
        self.enqueue_scoped(task, channel_id, 0, 0)
    }

    pub fn enqueue_scoped(&self, task: &str, channel_id: &str, guild: u64, user: u64) -> Result<i64, String> {
        self.0.execute("INSERT INTO jobs (task, channel_id, guild, user) VALUES (?1, ?2, ?3, ?4)", params![task, channel_id, guild.to_string(), user.to_string()]).map_err(|error| error.to_string())?;
        Ok(self.0.last_insert_rowid())
    }

    pub fn finish(&self, id: i64, ok: bool, result: &str) -> Result<(), String> {
        self.0.execute("UPDATE jobs SET status=?2, result=?3 WHERE id=?1", params![id, if ok { "done" } else { "failed" }, result]).map(|_| ()).map_err(|error| error.to_string())
    }

    /// Manual retry of a failed job: back to the queue, same task.
    pub fn retry(&self, id: i64) -> Result<bool, String> {
        self.0.execute("UPDATE jobs SET status='queued', result=NULL, delivered=0 WHERE id=?1 AND status='failed'", params![id]).map(|changed| changed == 1).map_err(|error| error.to_string())
    }

    /// Oldest queued job, marked running so nothing else picks it up.
    pub fn claim(&self) -> Result<Option<Job>, String> {
        let Some(job) = self.with_status(&["queued"])?.into_iter().next() else { return Ok(None) };
        self.0.execute("UPDATE jobs SET status='running' WHERE id=?1", params![job.id]).map_err(|error| error.to_string())?;
        Ok(Some(Job { status: "running".into(), ..job }))
    }

    pub fn with_status(&self, statuses: &[&str]) -> Result<Vec<Job>, String> {
        let mut statement = self.0.prepare("SELECT id, task, channel_id, status, guild, user, result FROM jobs ORDER BY id").map_err(|error| error.to_string())?;
        let rows = statement.query_map([], job).map_err(|error| error.to_string())?;
        Ok(rows.filter_map(Result::ok).filter(|job| statuses.contains(&job.status.as_str())).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrates_old_jobs_and_preserves_unsigned_discord_context() {
        let path = std::env::temp_dir().join(format!("wallis-job-migration-{}.sqlite",std::process::id()));
        let db = Connection::open(&path).unwrap();
        db.execute_batch("CREATE TABLE jobs (id INTEGER PRIMARY KEY, task TEXT NOT NULL, channel_id TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'queued', result TEXT); INSERT INTO jobs(task,channel_id) VALUES('old','10')").unwrap();
        drop(db);
        let jobs = Jobs::open(path.to_str().unwrap()).unwrap();
        assert_eq!((jobs.claim().unwrap().unwrap().guild,jobs.with_status(&["running"]).unwrap()[0].user),(0,0));
        jobs.enqueue_scoped("music","10",u64::MAX,42).unwrap();
        drop(jobs);
        let jobs = Jobs::open(path.to_str().unwrap()).unwrap();
        let claimed = jobs.claim().unwrap().unwrap();
        assert_eq!((claimed.guild,claimed.user),(u64::MAX,42));
        drop(jobs); std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn restart_marks_unfinished_jobs_failed_instead_of_rerunning_them() {
        let jobs = Jobs::open(":memory:").unwrap();
        let (a, b, c) = (jobs.enqueue("one", "1").unwrap(), jobs.enqueue("two", "1").unwrap(), jobs.enqueue("three", "2").unwrap());
        jobs.finish(a, true, "ok").unwrap();
        let interrupted = jobs.fail_interrupted().unwrap();
        assert_eq!(interrupted.iter().map(|job| job.id).collect::<Vec<_>>(), vec![b, c]);
        assert_eq!(jobs.with_status(&["failed"]).unwrap().len(), 2);
        assert!(jobs.fail_interrupted().unwrap().is_empty(), "idempotent");
        assert!(jobs.retry(b).unwrap() && !jobs.retry(a).unwrap(), "only failed jobs can be retried");
        assert_eq!(jobs.claim().unwrap().map(|job| (job.id, job.status)), Some((b, "running".into())));
        assert!(jobs.claim().unwrap().is_none(), "a claimed job is not handed out twice");
    }

    #[test]
    fn finished_jobs_are_handed_back_once() {
        let jobs = Jobs::open(":memory:").unwrap();
        let (a, b) = (jobs.enqueue("one", "1").unwrap(), jobs.enqueue("two", "1").unwrap());
        jobs.finish(b, false, "broke").unwrap();
        jobs.finish(a, true, "3 jokes").unwrap();
        let back = jobs.take_finished().unwrap();
        assert_eq!(back.iter().map(|job| (job.id, job.result.as_deref())).collect::<Vec<_>>(), [(a, Some("3 jokes")), (b, Some("broke"))]);
        assert!(jobs.take_finished().unwrap().is_empty());
        assert!(jobs.retry(b).unwrap());
        jobs.claim().unwrap();
        jobs.finish(b, true, "fixed").unwrap();
        assert_eq!(jobs.take_finished().unwrap().len(), 1, "a retried job reports again");
    }
}
