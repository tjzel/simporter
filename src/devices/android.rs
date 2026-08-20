use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};

use super::{Device, Platform, run_capture};
use crate::config::Config;

pub fn discover() -> Result<Vec<Device>> {
    let emulator = tool("emulator/emulator")?;
    let listed = run_capture(&emulator, &["-list-avds"])?;
    let booted = booted_avds().unwrap_or_default();
    let devices = listed
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.contains(' '))
        .map(|name| Device {
            id: name.to_string(),
            name: name.to_string(),
            platform: Platform::Android,
            running: booted.contains_key(name),
            managed: false,
            serial: booted.get(name).cloned(),
        })
        .collect();
    Ok(devices)
}

pub fn create_command(
    config: &Config,
    name: &str,
    device_profile: Option<&str>,
    system_image: Option<&str>,
) -> Result<String> {
    let avdmanager = tool("cmdline-tools/latest/bin/avdmanager")?;
    let package = match system_image {
        Some(value) => value.to_string(),
        None => newest_system_image()?,
    };
    let profile = device_profile
        .or(config.default_avd_profile.as_deref())
        .map(|value| format!(" -d '{value}'"))
        .unwrap_or_default();
    Ok(format!(
        "echo no | '{avdmanager}' create avd -n '{name}' -k '{package}'{profile}"
    ))
}

fn booted_avds() -> Result<HashMap<String, String>> {
    let adb = tool("platform-tools/adb")?;
    let listed = run_capture(&adb, &["devices"])?;
    let mut mapping = HashMap::new();
    for line in listed.lines().skip(1) {
        let mut parts = line.split_whitespace();
        let (Some(serial), Some("device")) = (parts.next(), parts.next()) else {
            continue;
        };
        if let Ok(name) = avd_name(&adb, serial) {
            mapping.insert(name, serial.to_string());
        }
    }
    Ok(mapping)
}

fn avd_name(adb: &str, serial: &str) -> Result<String> {
    let raw = run_capture(adb, &["-s", serial, "emu", "avd", "name"])?;
    raw.lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && *line != "OK")
        .map(str::to_string)
        .with_context(|| format!("could not read the AVD name of {serial}"))
}

fn newest_system_image() -> Result<String> {
    let root = sdk_root()?.join("system-images");
    let mut candidates = Vec::new();
    for api in read_dir_names(&root)? {
        for tag in read_dir_names(&root.join(&api))? {
            for abi in read_dir_names(&root.join(&api).join(&tag))? {
                if !root
                    .join(&api)
                    .join(&tag)
                    .join(&abi)
                    .join("system.img")
                    .exists()
                {
                    continue;
                }
                candidates.push((
                    api_key(&api),
                    tag_rank(&tag),
                    abi_rank(&abi),
                    api.clone(),
                    tag.clone(),
                    abi,
                ));
                break;
            }
        }
    }
    candidates.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)).then(b.2.cmp(&a.2)));
    let Some((.., api, tag, abi)) = candidates.into_iter().next() else {
        bail!(
            "no Android system image is installed under {}",
            root.display()
        );
    };
    Ok(format!("system-images;{api};{tag};{abi}"))
}

fn read_dir_names(path: &PathBuf) -> Result<Vec<String>> {
    if !path.is_dir() {
        return Ok(Vec::new());
    }
    let mut names = Vec::new();
    for entry in std::fs::read_dir(path)
        .with_context(|| format!("failed to list {}", path.display()))?
        .flatten()
    {
        if entry.path().is_dir()
            && let Some(name) = entry.file_name().to_str()
        {
            names.push(name.to_string());
        }
    }
    Ok(names)
}

fn api_key(api: &str) -> (u32, u32) {
    let digits = api.strip_prefix("android-").unwrap_or(api);
    let mut parts = digits
        .split('.')
        .map(|part| part.parse::<u32>().unwrap_or(0));
    (parts.next().unwrap_or(0), parts.next().unwrap_or(0))
}

fn tag_rank(tag: &str) -> u32 {
    match tag {
        "google_apis" => 3,
        "google_apis_playstore" => 2,
        "default" => 1,
        _ => 0,
    }
}

fn abi_rank(abi: &str) -> u32 {
    let native = if cfg!(target_arch = "aarch64") {
        "arm64-v8a"
    } else {
        "x86_64"
    };
    u32::from(abi == native)
}

pub fn tool(relative: &str) -> Result<String> {
    if let Ok(root) = sdk_root() {
        let path = root.join(relative);
        if path.exists() {
            return Ok(path.to_string_lossy().into_owned());
        }
    }
    let fallback = relative.rsplit('/').next().unwrap_or(relative);
    which(fallback).with_context(|| {
        format!("could not find `{fallback}`; set ANDROID_HOME to your Android SDK")
    })
}

fn sdk_root() -> Result<PathBuf> {
    for key in ["ANDROID_HOME", "ANDROID_SDK_ROOT"] {
        if let Some(value) = std::env::var_os(key) {
            let path = PathBuf::from(value);
            if path.is_dir() {
                return Ok(path);
            }
        }
    }
    let default = dirs::home_dir()
        .context("could not determine the home directory")?
        .join("Library/Android/sdk");
    if default.is_dir() {
        return Ok(default);
    }
    bail!("no Android SDK found; set ANDROID_HOME")
}

fn which(program: &str) -> Option<String> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
        .map(|candidate| candidate.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_key_orders_levels_numerically() {
        assert!(api_key("android-37.0") > api_key("android-36"));
        assert!(api_key("android-36") > api_key("android-34"));
        assert!(api_key("android-10") > api_key("android-9"));
    }

    #[test]
    fn google_apis_outranks_playstore_and_default() {
        assert!(tag_rank("google_apis") > tag_rank("google_apis_playstore"));
        assert!(tag_rank("google_apis_playstore") > tag_rank("default"));
        assert_eq!(tag_rank("google_apis_ps16k"), 0);
    }

    #[test]
    fn native_abi_is_preferred() {
        let native = if cfg!(target_arch = "aarch64") {
            "arm64-v8a"
        } else {
            "x86_64"
        };
        assert!(abi_rank(native) > abi_rank("armeabi-v7a"));
    }
}
