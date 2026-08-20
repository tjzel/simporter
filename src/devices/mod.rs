pub mod android;
pub mod ios;

use std::path::{Path, PathBuf};
use std::process::Command;

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

pub fn run_capture(program: &str, args: &[&str]) -> Result<String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("failed to run `{program} {}`", args.join(" ")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("`{program} {}` failed: {}", args.join(" "), stderr.trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
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
}
