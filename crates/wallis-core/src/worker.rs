//! Runs queued jobs on the worker agent, one at a time. The result is stored on the job; the brain hands it privately to Wallis,
//! so the user hears about it from him, not from a channel post.
use crate::agent::Agent;
use crate::host::Discord;
use crate::jobs::{Job, Jobs};
use std::time::Duration;

/// Claims and runs at most one job. Returns the job when one ran. `start` builds a fresh worker agent per job,
/// so a stuck or poisoned worker never leaks into the next task.
pub fn run_next(jobs: &Jobs, discord: &mut impl Discord, start: &mut dyn FnMut() -> Result<Agent, String>, idle_timeout: Duration, data: &std::path::Path) -> Result<Option<Job>, String> {
    let Some(job) = jobs.claim()? else { return Ok(None) };
    discord.scope(job.guild, job.user);
    let outcome = (|| {
        crate::mods::check_budget(data)?;
        let reminders = crate::schedule::Reminders::open(&data.join("jobs.sqlite").to_string_lossy())?;
        let mut host = crate::host::Host { discord, jobs, reminders: &reminders, guild: job.guild, user: job.user, write_channels: vec![job.channel_id.clone()], skipped: false, profile: data.join("profile.json") };
        let mut agent = start()?;
        agent.turn(&format!("Guild: {}. Requester: {}. Reply channel: {}.\n{}", job.guild, job.user, job.channel_id, job.task), idle_timeout, &mut |call| {
            if matches!(call.tool.as_str(), "queue_list" | "skip_audio" | "stop_audio" | "volume") { host.handle(call) } else { "Failed: This tool is unavailable to the worker.".into() }
        }, &mut |_| {})
    })();
    let (ok, result) = match outcome { Ok(text) if !text.trim().is_empty() => (true, text), Ok(_) => (false, "The worker finished without a result.".into()), Err(error) => (false, error) };
    jobs.finish(job.id, ok, &result)?;
    Ok(Some(job))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake;
    impl Discord for Fake {
        fn say(&mut self, _: &str) -> Result<(), String> { Ok(()) }
        fn message(&mut self, _: &str, _: &str) -> Result<(), String> { Err("the worker never posts".into()) }
        fn file(&mut self, _: &str, _: &str, _: &str, _: &str) -> Result<(), String> { Err("the worker never posts".into()) }
    }

    #[test]
    fn a_failed_start_stores_the_failure_for_wallis_and_posts_nothing() {
        let (jobs, mut fake) = (Jobs::open(":memory:").unwrap(), Fake);
        let id = jobs.enqueue("do it", "10").unwrap();
        let data = std::env::temp_dir().join(format!("wallis-worker-{}", std::process::id()));
        std::fs::create_dir_all(data.join("workspace")).unwrap();
        let ran = run_next(&jobs, &mut fake, &mut || Err("Pi missing".into()), Duration::from_secs(1), &data).unwrap();
        assert_eq!(ran.map(|job| job.id), Some(id));
        let finished = jobs.take_finished().unwrap();
        assert_eq!((finished.len(), finished[0].status.as_str(), finished[0].result.as_deref()), (1, "failed", Some("Pi missing")));
        assert!(run_next(&jobs, &mut fake, &mut || Err("x".into()), Duration::from_secs(1), &data).unwrap().is_none());
        std::fs::remove_dir_all(data).unwrap();
    }
}
