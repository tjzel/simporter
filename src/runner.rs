use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};

use crate::cli::{RunArgs, parse_duration, resolve_owner};
use crate::commands::{self, LeaseOutcome};
use crate::config::Config;
use crate::exit;
use crate::state::Claim;

pub fn run(config: &Config, args: &RunArgs) -> Result<i32> {
    let owner = resolve_owner(args.lease.owner.as_deref())?;
    let ttl = match args.lease.ttl.as_deref() {
        Some(raw) => parse_duration(raw)?,
        None => config.default_ttl_secs,
    };
    let mut lease = args.lease.clone();
    lease.owner_pid = Some(std::process::id());
    let outcome = commands::with_state(config, |state, now| {
        commands::acquire(config, state, now, &lease, &owner, None)
    })?;
    let claim = match outcome {
        LeaseOutcome::Claimed {
            ref claim,
            ref warnings,
        } => {
            for warning in warnings {
                eprintln!("simporter: {warning}");
            }
            claim.clone()
        }
        LeaseOutcome::PoolExhausted { .. } => {
            return Ok(commands::report_lease(config, &outcome, args.output));
        }
    };

    announce(&claim, args.output.json);
    let stop = Arc::new(AtomicBool::new(false));
    let heartbeat = spawn_heartbeat(config, &claim, ttl, &stop);
    let status = execute(&args.command, &claim);
    stop.store(true, Ordering::Relaxed);
    let _ = heartbeat.join();
    let released = commands::with_state(config, |state, _| {
        state.claims.retain(|entry| entry.id != claim.id);
        Ok(())
    });
    if let Err(error) = released {
        eprintln!("simporter: failed to release {}: {error:#}", claim.id);
    }
    status
}

fn execute(command: &[String], claim: &Claim) -> Result<i32> {
    let (program, rest) = command.split_first().context("no command given after --")?;
    let mut child = Command::new(program);
    child.args(rest);
    for (key, value) in commands::environment(claim) {
        child.env(key, value);
    }
    let status = child
        .status()
        .with_context(|| format!("failed to start `{}`", command.join(" ")))?;
    Ok(exit_code(status))
}

fn spawn_heartbeat(
    config: &Config,
    claim: &Claim,
    ttl: u64,
    stop: &Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    let interval = (ttl / 3).max(5);
    let stop = Arc::clone(stop);
    let config = config.clone();
    let claim_id = claim.id.clone();
    std::thread::spawn(move || {
        let mut waited = 0;
        while !stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_secs(1));
            waited += 1;
            if waited < interval {
                continue;
            }
            waited = 0;
            let renewed = commands::with_state(&config, |state, now| {
                for entry in &mut state.claims {
                    if entry.id == claim_id {
                        entry.expires_at = now + ttl;
                    }
                }
                Ok(())
            });
            if let Err(error) = renewed {
                eprintln!("simporter: heartbeat for {claim_id} failed: {error:#}");
            }
        }
    })
}

fn announce(claim: &Claim, json: bool) {
    if json {
        if let Ok(body) = serde_json::to_string(&commands::claim_payload(claim, &[])) {
            eprintln!("{body}");
        }
        return;
    }
    let device = claim
        .device
        .as_ref()
        .map_or_else(|| "none".to_string(), |device| device.name.clone());
    let ports: Vec<String> = claim
        .ports
        .iter()
        .map(|port| format!("{}={}", port.name, port.port))
        .collect();
    eprintln!(
        "simporter: holding {} device={device} ports=[{}]",
        claim.id,
        ports.join(" ")
    );
}

pub fn exit_code(status: std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status
        .code()
        .unwrap_or_else(|| status.signal().map_or(exit::FAILURE, |signal| 128 + signal))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_code_passes_the_child_status_through() {
        let status = Command::new("sh").args(["-c", "exit 7"]).status().unwrap();
        assert_eq!(exit_code(status), 7);
    }

    #[test]
    fn exit_code_maps_a_signal_to_the_shell_convention() {
        let status = Command::new("sh")
            .args(["-c", "kill -KILL $$"])
            .status()
            .unwrap();
        assert_eq!(exit_code(status), 128 + libc::SIGKILL);
    }
}
