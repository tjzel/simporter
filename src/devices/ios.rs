use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use super::{Device, Platform, run_capture};
use crate::config::Config;

pub fn discover() -> Result<Vec<Device>> {
    let raw = run_capture("xcrun", &["simctl", "list", "-j", "devices", "available"])?;
    let parsed: DeviceList =
        serde_json::from_str(&raw).context("failed to parse `simctl list -j devices` output")?;
    let mut devices = Vec::new();
    for entries in parsed.devices.into_values() {
        for entry in entries {
            if !entry.is_available {
                continue;
            }
            devices.push(Device {
                id: entry.udid,
                name: entry.name,
                platform: Platform::Ios,
                running: entry.state == "Booted",
                managed: false,
                serial: None,
            });
        }
    }
    Ok(devices)
}

pub fn create_command(
    config: &Config,
    name: &str,
    device_type: Option<&str>,
    runtime: Option<&str>,
) -> Result<String> {
    let device_type = match device_type {
        Some(value) => resolve_device_type(value)?,
        None => resolve_device_type(&config.default_device_type)?,
    };
    let runtime = match runtime {
        Some(value) => resolve_runtime(value)?,
        None => newest_runtime()?,
    };
    Ok(format!(
        "xcrun simctl create '{name}' {device_type} {runtime}"
    ))
}

fn resolve_device_type(wanted: &str) -> Result<String> {
    let raw = run_capture("xcrun", &["simctl", "list", "-j", "devicetypes"])?;
    let parsed: DeviceTypeList = serde_json::from_str(&raw)
        .context("failed to parse `simctl list -j devicetypes` output")?;
    parsed
        .devicetypes
        .into_iter()
        .find(|entry| entry.name == wanted || entry.identifier == wanted)
        .map(|entry| entry.identifier)
        .with_context(|| format!("no simulator device type named {wanted:?} is installed"))
}

fn resolve_runtime(wanted: &str) -> Result<String> {
    available_runtimes()?
        .into_iter()
        .find(|entry| entry.identifier == wanted || entry.name == wanted || entry.version == wanted)
        .map(|entry| entry.identifier)
        .with_context(|| format!("no simulator runtime named {wanted:?} is installed"))
}

fn newest_runtime() -> Result<String> {
    let mut runtimes: Vec<Runtime> = available_runtimes()?
        .into_iter()
        .filter(|entry| entry.platform.as_deref() == Some("iOS"))
        .collect();
    if runtimes.is_empty() {
        bail!("no available iOS simulator runtime is installed");
    }
    runtimes.sort_by_key(|entry| version_key(&entry.version));
    Ok(runtimes.pop().expect("runtimes is not empty").identifier)
}

fn available_runtimes() -> Result<Vec<Runtime>> {
    let raw = run_capture("xcrun", &["simctl", "list", "-j", "runtimes"])?;
    let parsed: RuntimeList =
        serde_json::from_str(&raw).context("failed to parse `simctl list -j runtimes` output")?;
    Ok(parsed
        .runtimes
        .into_iter()
        .filter(|entry| entry.is_available)
        .collect())
}

fn version_key(version: &str) -> (u32, u32, u32) {
    let mut parts = version
        .split('.')
        .map(|part| part.parse::<u32>().unwrap_or(0));
    (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    )
}

#[derive(Debug, Deserialize)]
struct DeviceList {
    devices: BTreeMap<String, Vec<SimDevice>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SimDevice {
    name: String,
    udid: String,
    state: String,
    #[serde(default)]
    is_available: bool,
}

#[derive(Debug, Deserialize)]
struct DeviceTypeList {
    devicetypes: Vec<DeviceType>,
}

#[derive(Debug, Deserialize)]
struct DeviceType {
    name: String,
    identifier: String,
}

#[derive(Debug, Deserialize)]
struct RuntimeList {
    runtimes: Vec<Runtime>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Runtime {
    identifier: String,
    name: String,
    version: String,
    #[serde(default)]
    platform: Option<String>,
    #[serde(default)]
    is_available: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_list_parses_the_simctl_shape() {
        let raw = r#"{
          "devices": {
            "com.apple.CoreSimulator.SimRuntime.iOS-26-5": [
              {"name": "iPhone 17 Pro Agent 1", "udid": "AAAA", "state": "Booted", "isAvailable": true},
              {"name": "iPhone 17 Pro", "udid": "BBBB", "state": "Shutdown", "isAvailable": true},
              {"name": "Retired", "udid": "CCCC", "state": "Shutdown", "isAvailable": false}
            ]
          }
        }"#;
        let parsed: DeviceList = serde_json::from_str(raw).unwrap();
        let entries = &parsed.devices["com.apple.CoreSimulator.SimRuntime.iOS-26-5"];
        assert_eq!(entries.len(), 3);
        assert!(entries[0].is_available);
        assert_eq!(entries[0].state, "Booted");
        assert!(!entries[2].is_available);
    }

    #[test]
    fn version_key_orders_runtimes_numerically() {
        assert!(version_key("26.5") > version_key("18.10"));
        assert!(version_key("18.10") > version_key("18.4"));
    }

    #[test]
    fn runtime_list_parses_the_simctl_shape() {
        let raw = r#"{"runtimes": [
          {"identifier": "com.apple.CoreSimulator.SimRuntime.iOS-26-5", "name": "iOS 26.5",
           "version": "26.5", "platform": "iOS", "isAvailable": true},
          {"identifier": "com.apple.CoreSimulator.SimRuntime.tvOS-26-5", "name": "tvOS 26.5",
           "version": "26.5", "platform": "tvOS", "isAvailable": true}
        ]}"#;
        let parsed: RuntimeList = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.runtimes.len(), 2);
        assert_eq!(parsed.runtimes[0].platform.as_deref(), Some("iOS"));
    }
}
