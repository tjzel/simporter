use std::fmt::Write as _;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Serialize;

use crate::cli::{OutputArgs, ReaperArgs};
use crate::commands::with_state;
use crate::config::{self, Config};
use crate::devices::{self, Device, Platform, android, run_capture_within};
use crate::state::{ReapedDevice, State};
use crate::{clock, exit};

pub const LABEL: &str = "com.github.tjzel.simporter.reaper";

const SHUTDOWN_TIMEOUT: Duration = Duration::from_mins(2);
const KEPT_RECORDS: usize = 20;
const INTERVAL_SECS: u64 = 300;

pub fn reap(config: &Config) -> ReapReport {
    let mut report = ReapReport::default();
    if config.reap_unleased_after_secs == 0 {
        return report;
    }
    let found = match devices::discover(config, None) {
        Ok(found) => found,
        Err(error) => {
            report.error = Some(format!("{error:#}"));
            return report;
        }
    };
    let due = match with_state(config, |state, now| {
        Ok(track(state, &found, now, config.reap_unleased_after_secs))
    }) {
        Ok(due) => due,
        Err(error) => {
            report.error = Some(format!("{error:#}"));
            return report;
        }
    };
    for (device, since) in due {
        if let Some(record) = shut_down_if_still_unleased(config, &device, since) {
            report.shut_down.push(record);
        }
    }
    report
}

pub fn track(state: &mut State, found: &[Device], now: u64, after: u64) -> Vec<(Device, u64)> {
    let reserved: Vec<String> = state
        .reserved_names()
        .into_iter()
        .map(String::from)
        .collect();
    let mut due = Vec::new();
    for device in found.iter().filter(|device| device.managed) {
        if state.holder_of_device(&device.id).is_some() || reserved.contains(&device.name) {
            state.device_first_unleased.remove(&device.id);
            continue;
        }
        if !device.running {
            state.device_last_held.remove(&device.id);
            state.device_first_unleased.remove(&device.id);
            continue;
        }
        let since = unleased_since(state, &device.id).unwrap_or_else(|| {
            state.device_first_unleased.insert(device.id.clone(), now);
            now
        });
        if now >= since.saturating_add(after) {
            due.push((device.clone(), since));
        }
    }
    due
}

pub fn unleased_since(state: &State, device_id: &str) -> Option<u64> {
    state
        .device_last_held
        .get(device_id)
        .or_else(|| state.device_first_unleased.get(device_id))
        .copied()
}

pub fn run_installer(args: &ReaperArgs) -> Result<i32> {
    let plist_path = plist_path()?;
    if args.uninstall {
        if !args.dry_run {
            let _ = launchctl(&["bootout", &format!("{}/{LABEL}", domain())]);
            if plist_path.exists() {
                std::fs::remove_file(&plist_path)
                    .with_context(|| format!("failed to remove {}", plist_path.display()))?;
            }
        }
        report_install(args.output, "uninstalled", &plist_path, None, args.dry_run);
        return Ok(exit::OK);
    }
    let body = plist(&std::env::current_exe().context("could not find the simporter binary")?)?;
    if args.install && !args.dry_run {
        if let Some(parent) = plist_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        std::fs::write(&plist_path, &body)
            .with_context(|| format!("failed to write {}", plist_path.display()))?;
        let _ = launchctl(&["bootout", &format!("{}/{LABEL}", domain())]);
        launchctl(&["bootstrap", &domain(), &plist_path.to_string_lossy()])?;
        report_install(args.output, "installed", &plist_path, Some(&body), false);
        return Ok(exit::OK);
    }
    report_install(args.output, "planned", &plist_path, Some(&body), true);
    Ok(exit::OK)
}

fn shut_down_if_still_unleased(
    config: &Config,
    device: &Device,
    since: u64,
) -> Option<ReapedDevice> {
    let still_unleased = with_state(config, |state, _| {
        Ok(state.holder_of_device(&device.id).is_none())
    })
    .unwrap_or(false);
    if !still_unleased {
        return None;
    }
    let error = shut_down(device).err().map(|error| format!("{error:#}"));
    let record = ReapedDevice {
        id: device.id.clone(),
        name: device.name.clone(),
        platform: device.platform,
        unleased_since: since,
        at: clock::now(),
        error,
    };
    let saved = record.clone();
    let _ = with_state(config, move |state, _| {
        if saved.error.is_none() {
            state.device_last_held.remove(&saved.id);
            state.device_first_unleased.remove(&saved.id);
        }
        state.reaped.push(saved);
        let excess = state.reaped.len().saturating_sub(KEPT_RECORDS);
        state.reaped.drain(..excess);
        Ok(())
    });
    append_log(&record);
    Some(record)
}

fn shut_down(device: &Device) -> Result<()> {
    match device.platform {
        Platform::Ios => {
            run_capture_within(
                "xcrun",
                &["simctl", "shutdown", &device.id],
                SHUTDOWN_TIMEOUT,
            )?;
        }
        Platform::Android => {
            let serial = device
                .serial
                .as_deref()
                .context("the emulator has no adb serial")?;
            let adb = android::tool("platform-tools/adb")?;
            run_capture_within(&adb, &["-s", serial, "emu", "kill"], SHUTDOWN_TIMEOUT)?;
        }
    }
    Ok(())
}

fn append_log(record: &ReapedDevice) {
    let Ok(home) = config::ensure_home() else {
        return;
    };
    let line = log_line(record);
    let written = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(home.join("reaper.log"))
        .and_then(|mut file| writeln!(file, "{line}"));
    if let Err(error) = written {
        eprintln!("simporter: failed to write reaper.log: {error}");
    }
}

pub fn log_line(record: &ReapedDevice) -> String {
    let outcome = match &record.error {
        None => "shut down".to_string(),
        Some(error) => format!("failed to shut down ({error})"),
    };
    format!(
        "{} {outcome} {} ({}, {}): running without a lease since {}",
        clock::rfc3339(record.at),
        record.name,
        record.platform,
        record.id,
        clock::rfc3339(record.unleased_since)
    )
}

fn plist_path() -> Result<PathBuf> {
    Ok(dirs::home_dir()
        .context("could not determine the home directory")?
        .join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist")))
}

fn plist(exe: &Path) -> Result<String> {
    let home = config::home()?;
    let mut environment = vec![(
        "PATH".to_string(),
        std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin:/usr/sbin:/sbin".to_string()),
    )];
    for key in ["SIMPORTER_HOME", "ANDROID_HOME", "ANDROID_SDK_ROOT"] {
        if let Ok(value) = std::env::var(key) {
            environment.push((key.to_string(), value));
        }
    }
    let mut entries = String::new();
    for (key, value) in &environment {
        let _ = write!(
            entries,
            "\n    <key>{}</key><string>{}</string>",
            escape(key),
            escape(value)
        );
    }
    let log = escape(&home.join("reaper.launchd.log").to_string_lossy());
    Ok(format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{}</string>
    <string>gc</string>
  </array>
  <key>StartInterval</key><integer>{INTERVAL_SECS}</integer>
  <key>RunAtLoad</key><true/>
  <key>EnvironmentVariables</key>
  <dict>{entries}
  </dict>
  <key>StandardOutPath</key><string>{log}</string>
  <key>StandardErrorPath</key><string>{log}</string>
</dict>
</plist>
"#,
        escape(&exe.to_string_lossy())
    ))
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn domain() -> String {
    format!("gui/{}", unsafe { libc::getuid() })
}

fn launchctl(args: &[&str]) -> Result<String> {
    run_capture_within("launchctl", args, Duration::from_secs(30))
}

fn report_install(
    output: OutputArgs,
    status: &str,
    path: &Path,
    body: Option<&str>,
    dry_run: bool,
) {
    if output.json {
        let payload = InstallPayload {
            status,
            label: LABEL,
            plist: path.display().to_string(),
            dry_run,
        };
        if let Ok(text) = serde_json::to_string_pretty(&payload) {
            println!("{text}");
        }
        return;
    }
    println!("{status} {LABEL} at {}", path.display());
    if let Some(body) = body
        && dry_run
    {
        println!();
        print!("{body}");
        println!();
        println!("nothing was written; rerun with --install to apply");
    }
}

#[derive(Debug, Default, Serialize)]
pub struct ReapReport {
    pub shut_down: Vec<ReapedDevice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
struct InstallPayload<'a> {
    status: &'a str,
    label: &'static str,
    plist: String,
    dry_run: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Claim, ClaimedDevice, Reservation};

    fn device(id: &str, name: &str, running: bool, managed: bool) -> Device {
        Device {
            id: id.to_string(),
            name: name.to_string(),
            platform: Platform::Ios,
            running,
            managed,
            serial: None,
        }
    }

    fn claim_on(device_id: &str) -> Claim {
        Claim {
            id: "c1".to_string(),
            owner: "agent".to_string(),
            owner_pid: None,
            label: None,
            device: Some(ClaimedDevice {
                id: device_id.to_string(),
                name: "iPhone 17 Pro Agent 1".to_string(),
                platform: Platform::Ios,
                serial: None,
            }),
            ports: Vec::new(),
            created_at: 0,
            expires_at: 10_000,
        }
    }

    #[test]
    fn a_never_leased_device_gets_the_full_wait_from_its_first_sighting() {
        let mut state = State::default();
        let found = [device("A", "iPhone 17 Pro Agent 1", true, true)];
        assert!(track(&mut state, &found, 1000, 1800).is_empty());
        assert!(track(&mut state, &found, 2799, 1800).is_empty());
        let due = track(&mut state, &found, 2800, 1800);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].1, 1000);
    }

    #[test]
    fn the_wait_counts_from_the_end_of_the_last_lease() {
        let mut state = State::default();
        state.device_last_held.insert("A".to_string(), 500);
        let found = [device("A", "iPhone 17 Pro Agent 1", true, true)];
        let due = track(&mut state, &found, 2300, 1800);
        assert_eq!(due[0].1, 500);
    }

    #[test]
    fn a_human_device_is_never_due() {
        let mut state = State::default();
        let found = [device("H", "iPhone 17 Pro", true, false)];
        assert!(track(&mut state, &found, 0, 0).is_empty());
        assert!(track(&mut state, &found, 1_000_000, 0).is_empty());
        assert!(state.device_first_unleased.is_empty());
    }

    #[test]
    fn a_leased_device_is_never_due_and_its_clock_resets() {
        let mut state = State::default();
        state.device_first_unleased.insert("A".to_string(), 0);
        state.claims.push(claim_on("A"));
        let found = [device("A", "iPhone 17 Pro Agent 1", true, true)];
        assert!(track(&mut state, &found, 1_000_000, 1800).is_empty());
        assert!(!state.device_first_unleased.contains_key("A"));
    }

    #[test]
    fn a_device_with_a_reserved_name_is_not_due() {
        let mut state = State::default();
        state.reservations.push(Reservation {
            name: "iPhone 17 Pro Agent 3".to_string(),
            owner: "agent".to_string(),
            platform: Platform::Ios,
            expires_at: 10_000,
        });
        let found = [device("R", "iPhone 17 Pro Agent 3", true, true)];
        assert!(track(&mut state, &found, 0, 0).is_empty());
    }

    #[test]
    fn a_stopped_device_forgets_its_history() {
        let mut state = State::default();
        state.device_last_held.insert("A".to_string(), 1);
        state.device_first_unleased.insert("A".to_string(), 1);
        let found = [device("A", "iPhone 17 Pro Agent 1", false, true)];
        assert!(track(&mut state, &found, 1_000_000, 1800).is_empty());
        assert!(state.device_last_held.is_empty());
        assert!(state.device_first_unleased.is_empty());
    }

    #[test]
    fn an_expired_lease_starts_the_clock_at_its_expiry() {
        let mut state = State::default();
        let mut claim = claim_on("A");
        claim.expires_at = 700;
        state.claims.push(claim);
        state.gc(900);
        assert_eq!(state.device_last_held.get("A"), Some(&700));
    }

    #[test]
    fn the_plist_runs_gc_every_five_minutes_and_escapes_paths() {
        let body = plist(Path::new("/opt/a&b/simporter")).unwrap();
        assert!(body.contains("<string>/opt/a&amp;b/simporter</string>"));
        assert!(body.contains("<string>gc</string>"));
        assert!(body.contains("<integer>300</integer>"));
        assert!(body.contains(LABEL));
    }

    #[test]
    fn the_log_line_names_the_device_and_the_reason() {
        let line = log_line(&ReapedDevice {
            id: "A".to_string(),
            name: "iPhone 17 Pro Agent 1".to_string(),
            platform: Platform::Ios,
            unleased_since: 0,
            at: 1800,
            error: None,
        });
        assert!(
            line.contains("shut down iPhone 17 Pro Agent 1 (ios, A)"),
            "{line}"
        );
        assert!(
            line.contains("running without a lease since 1970-01-01T00:00:00Z"),
            "{line}"
        );
    }
}
