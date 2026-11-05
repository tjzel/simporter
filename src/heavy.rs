use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Serialize;

use crate::cli::{HeavyArgs, resolve_owner};
use crate::commands::with_state;
use crate::config::Config;
use crate::state::{HeavyJob, State};
use crate::{clock, exit, runner};

pub const JOB_ENV: &str = "SIMPORTER_HEAVY_JOB";

const POLL_INTERVAL: Duration = Duration::from_millis(500);

pub fn run(config: &Config, args: &HeavyArgs) -> Result<i32> {
    if let Some(outer) = std::env::var_os(JOB_ENV) {
        eprintln!(
            "simporter: already inside heavy job {}; running without another slot",
            outer.to_string_lossy()
        );
        return execute(&args.command, None);
    }
    if args.weight > config.heavy_slots {
        return Err(exit::usage(format!(
            "--weight {} is more than the {} heavy slots in the config",
            args.weight, config.heavy_slots
        )));
    }
    let owner = owner(args.owner.as_deref());
    let label = args
        .label
        .clone()
        .unwrap_or_else(|| default_label(&args.command));
    let id = enqueue(config, &owner, &label, args.weight)?;
    let waited = wait_for_slot(config, &id, &owner, &label, args.weight, args.output.json)?;
    announce_start(&id, &label, waited, args.output.json);
    let status = execute(&args.command, Some(&id));
    let released = with_state(config, |state, _| {
        state.heavy.retain(|job| job.id != id);
        Ok(())
    });
    if let Err(error) = released {
        eprintln!("simporter: failed to release heavy slot {id}: {error:#}");
    }
    status
}

pub fn status_payload(state: &State, slots: u32, now: u64) -> HeavyStatus {
    let jobs = state
        .heavy
        .iter()
        .map(|job| {
            let since = job.started_at.unwrap_or(job.queued_at);
            HeavyJobPayload {
                id: job.id.clone(),
                owner: job.owner.clone(),
                label: job.label.clone(),
                weight: job.weight,
                pid: job.pid,
                state: if job.started_at.is_some() {
                    "running"
                } else {
                    "waiting"
                },
                since: clock::rfc3339(since),
                elapsed_secs: now.saturating_sub(since),
                elapsed: clock::humanize(now.saturating_sub(since)),
            }
        })
        .collect();
    HeavyStatus {
        slots,
        in_use: state.heavy_in_use(),
        jobs,
    }
}

pub fn print_status(status: &HeavyStatus) {
    println!("heavy slots {}/{} in use", status.in_use, status.slots);
    for job in &status.jobs {
        println!(
            "heavy {:<7} for {:<8} weight={} owner={} {}",
            job.state, job.elapsed, job.weight, job.owner, job.label
        );
    }
}

fn owner(explicit: Option<&str>) -> String {
    resolve_owner(explicit).unwrap_or_else(|_| format!("pid-{}", parent_pid()))
}

fn parent_pid() -> u32 {
    u32::try_from(unsafe { libc::getppid() }).unwrap_or_default()
}

fn default_label(command: &[String]) -> String {
    command
        .first()
        .map(|program| program.rsplit('/').next().unwrap_or(program).to_string())
        .unwrap_or_default()
}

fn enqueue(config: &Config, owner: &str, label: &str, weight: u32) -> Result<String> {
    with_state(config, |state, now| {
        let id = state.next_heavy_id(now);
        state.heavy.push(new_job(&id, owner, label, weight, now));
        Ok(id)
    })
}

fn new_job(id: &str, owner: &str, label: &str, weight: u32, now: u64) -> HeavyJob {
    HeavyJob {
        id: id.to_string(),
        owner: owner.to_string(),
        label: label.to_string(),
        weight,
        pid: std::process::id(),
        queued_at: now,
        started_at: None,
    }
}

fn wait_for_slot(
    config: &Config,
    id: &str,
    owner: &str,
    label: &str,
    weight: u32,
    json: bool,
) -> Result<u64> {
    let mut last_seen: Option<(u32, usize)> = None;
    loop {
        let progress = with_state(config, |state, now| {
            if !state.heavy.iter().any(|job| job.id == id) {
                state.heavy.push(new_job(id, owner, label, weight, now));
            }
            if state.heavy_can_start(id, config.heavy_slots) {
                let job = state
                    .heavy
                    .iter_mut()
                    .find(|job| job.id == id)
                    .context("heavy job vanished from the queue")?;
                job.started_at = Some(now);
                return Ok(Progress::Started(now.saturating_sub(job.queued_at)));
            }
            Ok(Progress::Waiting(status_payload(
                state,
                config.heavy_slots,
                now,
            )))
        })?;
        match progress {
            Progress::Started(waited) => return Ok(waited),
            Progress::Waiting(status) => {
                let ahead = status
                    .jobs
                    .iter()
                    .take_while(|job| job.id != id)
                    .filter(|job| job.state == "waiting")
                    .count();
                if last_seen != Some((status.in_use, ahead)) {
                    last_seen = Some((status.in_use, ahead));
                    announce_wait(id, &status, ahead, json);
                }
            }
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

fn announce_wait(id: &str, status: &HeavyStatus, ahead: usize, json: bool) {
    let running: Vec<&HeavyJobPayload> = status
        .jobs
        .iter()
        .filter(|job| job.state == "running")
        .collect();
    if json {
        let event = WaitEvent {
            event: "waiting",
            job_id: id,
            slots: status.slots,
            in_use: status.in_use,
            ahead,
            running: &running,
        };
        if let Ok(body) = serde_json::to_string(&event) {
            eprintln!("{body}");
        }
        return;
    }
    eprintln!(
        "simporter: waiting for a heavy slot: {}/{} in use, {ahead} waiting ahead of you",
        status.in_use, status.slots
    );
    for job in running {
        eprintln!(
            "simporter:   running for {} weight={} owner={} {}",
            job.elapsed, job.weight, job.owner, job.label
        );
    }
}

fn announce_start(id: &str, label: &str, waited: u64, json: bool) {
    if json {
        let event = StartEvent {
            event: "started",
            job_id: id,
            label,
            waited_secs: waited,
        };
        if let Ok(body) = serde_json::to_string(&event) {
            eprintln!("{body}");
        }
        return;
    }
    eprintln!(
        "simporter: holding heavy slot {id} for {label} after waiting {}",
        clock::humanize(waited)
    );
}

fn execute(command: &[String], job_id: Option<&str>) -> Result<i32> {
    let (program, rest) = command.split_first().context("no command given after --")?;
    let mut child = Command::new(program);
    child.args(rest);
    if let Some(id) = job_id {
        child.env(JOB_ENV, id);
    }
    let status = child
        .status()
        .with_context(|| format!("failed to start `{}`", command.join(" ")))?;
    Ok(runner::exit_code(status))
}

enum Progress {
    Started(u64),
    Waiting(HeavyStatus),
}

#[derive(Debug, Serialize)]
pub struct HeavyStatus {
    slots: u32,
    in_use: u32,
    jobs: Vec<HeavyJobPayload>,
}

#[derive(Debug, Serialize)]
struct HeavyJobPayload {
    id: String,
    owner: String,
    label: String,
    weight: u32,
    pid: u32,
    state: &'static str,
    since: String,
    elapsed_secs: u64,
    elapsed: String,
}

#[derive(Debug, Serialize)]
struct WaitEvent<'a> {
    event: &'static str,
    job_id: &'a str,
    slots: u32,
    in_use: u32,
    ahead: usize,
    running: &'a [&'a HeavyJobPayload],
}

#[derive(Debug, Serialize)]
struct StartEvent<'a> {
    event: &'static str,
    job_id: &'a str,
    label: &'a str,
    waited_secs: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(id: &str, weight: u32, started: bool) -> HeavyJob {
        HeavyJob {
            id: id.to_string(),
            owner: "owner".to_string(),
            label: "build".to_string(),
            weight,
            pid: std::process::id(),
            queued_at: 100,
            started_at: started.then_some(110),
        }
    }

    fn state(jobs: Vec<HeavyJob>) -> State {
        State {
            heavy: jobs,
            ..State::default()
        }
    }

    #[test]
    fn the_first_waiting_job_starts_when_its_weight_fits() {
        let state = state(vec![job("a", 1, true), job("b", 1, false)]);
        assert!(state.heavy_can_start("b", 2));
        assert!(!state.heavy_can_start("b", 1));
    }

    #[test]
    fn a_job_behind_the_head_of_the_queue_never_jumps_it() {
        let state = state(vec![
            job("a", 1, true),
            job("heavy", 2, false),
            job("light", 1, false),
        ]);
        assert!(!state.heavy_can_start("heavy", 2));
        assert!(!state.heavy_can_start("light", 2));
    }

    #[test]
    fn a_running_job_cannot_start_again() {
        let state = state(vec![job("a", 1, true)]);
        assert!(!state.heavy_can_start("a", 2));
    }

    #[test]
    fn the_label_defaults_to_the_program_name() {
        let command = vec!["/usr/bin/xcodebuild".to_string(), "-quiet".to_string()];
        assert_eq!(default_label(&command), "xcodebuild");
    }

    #[test]
    fn status_reports_slots_in_use_and_job_states() {
        let state = state(vec![job("a", 2, true), job("b", 1, false)]);
        let status = status_payload(&state, 2, 130);
        assert_eq!(status.in_use, 2);
        assert_eq!(status.jobs[0].state, "running");
        assert_eq!(status.jobs[0].elapsed_secs, 20);
        assert_eq!(status.jobs[1].state, "waiting");
        assert_eq!(status.jobs[1].elapsed_secs, 30);
    }
}
