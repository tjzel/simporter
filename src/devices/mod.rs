pub mod android;
pub mod ios;

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::naming;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
#[value(rename_all = "lowercase")]
pub enum Platform {
    Ios,
    Android,
}

impl Platform {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ios => "ios",
            Self::Android => "android",
        }
    }
}

impl std::fmt::Display for Platform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Deserialize)]
struct ScriptedDevice {
    id: String,
    name: String,
    platform: Platform,
    #[serde(default)]
    running: bool,
    #[serde(default)]
    serial: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub platform: Platform,
    pub running: bool,
    pub managed: bool,
    pub serial: Option<String>,
}

pub fn discover(config: &Config, platform: Option<Platform>) -> Result<Vec<Device>> {
    if let Some(path) = std::env::var_os("SIMPORTER_DEVICE_SOURCE") {
        return scripted(config, platform, &PathBuf::from(path));
    }
    let mut devices = Vec::new();
    let mut failures = Vec::new();
    for candidate in [Platform::Ios, Platform::Android] {
        if platform.is_some_and(|wanted| wanted != candidate) {
            continue;
        }
        match discover_one(config, candidate) {
            Ok(found) => devices.extend(found),
            Err(error) => failures.push(format!("{candidate}: {error:#}")),
        }
    }
    if devices.is_empty() && !failures.is_empty() {
        bail!("device discovery failed ({})", failures.join("; "));
    }
    Ok(devices)
}

pub fn find(config: &Config, id: &str) -> Result<Device> {
    discover(config, None)?
        .into_iter()
        .find(|device| device.id == id || device.name == id)
        .with_context(|| format!("no simulator or emulator matches {id:?}"))
}

pub fn create_plan(
    config: &Config,
    platform: Platform,
    name: &str,
    device_type: Option<&str>,
    runtime: Option<&str>,
) -> Result<String> {
    match platform {
        Platform::Ios => ios::create_command(config, name, device_type, runtime),
        Platform::Android => android::create_command(config, name, None, runtime),
    }
}

fn scripted(config: &Config, platform: Option<Platform>, path: &Path) -> Result<Vec<Device>> {
    let raw = std::fs::read_to_string(path).with_context(|| {
        format!(
            "failed to read SIMPORTER_DEVICE_SOURCE at {}",
            path.display()
        )
    })?;
    let entries: Vec<ScriptedDevice> = serde_json::from_str(&raw).with_context(|| {
        format!(
            "failed to parse SIMPORTER_DEVICE_SOURCE at {}",
            path.display()
        )
    })?;
    Ok(entries
        .into_iter()
        .filter(|entry| platform.is_none_or(|wanted| wanted == entry.platform))
        .map(|entry| Device {
            managed: naming::is_managed(config, entry.platform, &entry.name),
            id: entry.id,
            name: entry.name,
            platform: entry.platform,
            running: entry.running,
            serial: entry.serial,
        })
        .collect())
}

fn discover_one(config: &Config, platform: Platform) -> Result<Vec<Device>> {
    let mut devices = match platform {
        Platform::Ios => ios::discover()?,
        Platform::Android => android::discover()?,
    };
    for device in &mut devices {
        device.managed = naming::is_managed(config, platform, &device.name);
    }
    Ok(devices)
}

const COMMAND_TIMEOUT: Duration = Duration::from_mins(1);

pub fn run_capture(program: &str, args: &[&str]) -> Result<String> {
    run_capture_within(program, args, COMMAND_TIMEOUT)
}

pub fn run_capture_within(program: &str, args: &[&str], timeout: Duration) -> Result<String> {
    let shown = format!("{program} {}", args.join(" "));
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to run `{shown}`"))?;
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("`{shown}` did not finish within {}s", timeout.as_secs());
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let stdout = stdout.join().unwrap_or_default();
    let stderr = stderr.join().unwrap_or_default();
    if !status.success() {
        bail!("`{shown}` failed: {}", stderr.trim());
    }
    Ok(stdout)
}

fn drain(pipe: Option<impl Read + Send + 'static>) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut text = String::new();
        if let Some(mut pipe) = pipe {
            let mut bytes = Vec::new();
            let _ = pipe.read_to_end(&mut bytes);
            text = String::from_utf8_lossy(&bytes).into_owned();
        }
        text
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_round_trips_through_json() {
        let encoded = serde_json::to_string(&Platform::Android).unwrap();
        assert_eq!(encoded, "\"android\"");
        let decoded: Platform = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, Platform::Android);
    }

    #[test]
    fn run_capture_reports_a_failing_command() {
        let error = run_capture("false", &[]).unwrap_err();
        assert!(error.to_string().contains("failed"));
    }

    #[test]
    fn run_capture_stops_a_command_that_hangs() {
        let started = Instant::now();
        let error = run_capture_within("sleep", &["30"], Duration::from_millis(300)).unwrap_err();
        assert!(error.to_string().contains("did not finish"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn run_capture_returns_the_output() {
        let output = run_capture_within("sh", &["-c", "echo hello"], COMMAND_TIMEOUT).unwrap();
        assert_eq!(output, "hello\n");
    }
}
