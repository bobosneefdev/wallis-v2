//! Runs queued jobs on the worker agent, one at a time, and reports each result through `Discord::message`.
use crate::agent::Agent;
use crate::host::Discord;
use crate::jobs::{Job, Jobs};
use std::time::Duration;

/// Discord limits a message to 2000 characters; longer results go out as a file.
const MESSAGE_LIMIT: usize = 1900;

/// Claims and runs at most one job. Returns the job when one ran. `start` builds a fresh worker agent per job,
/// so a stuck or poisoned worker never leaks into the next task.
pub fn run_next(jobs: &Jobs, discord: &mut impl Discord, start: &mut dyn FnMut() -> Result<Agent, String>, idle_timeout: Duration) -> Result<Option<Job>, String> {
    let Some(job) = jobs.claim()? else { return Ok(None) };
    let outcome = start().and_then(|mut agent| agent.turn(&job.task, idle_timeout, &mut |_| "The worker has no host tools.".into(), &mut |_| {}));
    let (ok, result) = match outcome { Ok(text) if !text.trim().is_empty() => (true, text), Ok(_) => (false, "The worker finished without a result.".into()), Err(error) => (false, error) };
    jobs.finish(job.id, ok, &result)?;
    let heading = if ok { format!("Job {} finished.", job.id) } else { format!("Job {} failed: {}", job.id, result.lines().next().unwrap_or("")) };
    let sent = if ok && result.len() > MESSAGE_LIMIT { discord.file(&job.channel_id, &format!("job-{}.md", job.id), &result, &heading) } else if ok { discord.message(&job.channel_id, &format!("{heading}\n\n{result}")) } else { discord.message(&job.channel_id, &heading) };
    sent.map_err(|error| format!("Job {} ran but its result could not be posted: {error}", job.id))?;
    Ok(Some(job))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Fake { messages: Vec<String>, files: Vec<String> }
    impl Discord for Fake {
        fn say(&mut self, _: &str) -> Result<(), String> { Ok(()) }
        fn message(&mut self, _: &str, text: &str) -> Result<(), String> { self.messages.push(text.into()); Ok(()) }
        fn file(&mut self, _: &str, name: &str, _: &str, _: &str) -> Result<(), String> { self.files.push(name.into()); Ok(()) }
    }

    #[test]
    fn a_failed_start_marks_the_job_failed_and_tells_the_channel() {
        let (jobs, mut fake) = (Jobs::open(":memory:").unwrap(), Fake::default());
        let id = jobs.enqueue("do it", "10").unwrap();
        let ran = run_next(&jobs, &mut fake, &mut || Err("Pi missing".into()), Duration::from_secs(1)).unwrap();
        assert_eq!(ran.map(|job| job.id), Some(id));
        assert_eq!(jobs.with_status(&["failed"]).unwrap().len(), 1);
        assert_eq!(fake.messages, vec![format!("Job {id} failed: Pi missing")]);
        assert!(run_next(&jobs, &mut fake, &mut || Err("x".into()), Duration::from_secs(1)).unwrap().is_none());
    }
}
