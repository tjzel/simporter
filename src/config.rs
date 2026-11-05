use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub agent_suffix: String,
    pub default_device_type: String,
    pub default_avd_base: String,
    pub default_avd_profile: Option<String>,
    pub port_range_start: u16,
    pub port_range_end: u16,
    pub default_ttl_secs: u64,
    pub reservation_ttl_secs: u64,
    pub lock_timeout_secs: u64,
    pub heavy_slots: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            agent_suffix: "Agent".to_string(),
            default_device_type: "iPhone 17 Pro".to_string(),
            default_avd_base: "Pixel_9".to_string(),
            default_avd_profile: None,
            port_range_start: 8081,
            port_range_end: 8199,
            default_ttl_secs: 1800,
            reservation_ttl_secs: 600,
            lock_timeout_secs: 10,
            heavy_slots: 2,
        }
    }
}

pub fn load() -> Result<Config> {
    let path = config_path()?;
    if !path.exists() {
        let config = Config::default();
        write_defaults(&path, &config)?;
        return Ok(config);
    }
    let raw = fs::read_to_string(&path)
        .with_context(|| format!("failed to read config at {}", path.display()))?;
    let config: Config = serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse config at {}", path.display()))?;
    config.validate()?;
    Ok(config)
}

impl Config {
    fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            !self.agent_suffix.is_empty(),
            "agent_suffix must not be empty"
        );
        anyhow::ensure!(
            self.port_range_start <= self.port_range_end,
            "port_range_start ({}) must not exceed port_range_end ({})",
            self.port_range_start,
            self.port_range_end
        );
        anyhow::ensure!(
            self.default_ttl_secs > 0,
            "default_ttl_secs must be greater than zero"
        );
        anyhow::ensure!(
            self.heavy_slots > 0,
            "heavy_slots must be greater than zero"
        );
        Ok(())
    }
}

pub fn home() -> Result<PathBuf> {
    if let Some(raw) = std::env::var_os("SIMPORTER_HOME") {
        return Ok(PathBuf::from(raw));
    }
    let base = dirs::home_dir().context("could not determine the home directory")?;
    Ok(base.join(".simporter"))
}

pub fn ensure_home() -> Result<PathBuf> {
    let dir = home()?;
    fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create state directory {}", dir.display()))?;
    Ok(dir)
}

pub fn state_path() -> Result<PathBuf> {
    Ok(ensure_home()?.join("state.json"))
}

pub fn lock_path() -> Result<PathBuf> {
    Ok(ensure_home()?.join("state.lock"))
}

fn config_path() -> Result<PathBuf> {
    Ok(ensure_home()?.join("config.json"))
}

fn write_defaults(path: &Path, config: &Config) -> Result<()> {
    let body = serde_json::to_string_pretty(config)?;
    crate::state::write_atomic(path, &body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid() {
        Config::default().validate().unwrap();
    }

    #[test]
    fn inverted_port_range_is_rejected() {
        let config = Config {
            port_range_start: 9000,
            port_range_end: 8000,
            ..Config::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn empty_suffix_is_rejected() {
        let config = Config {
            agent_suffix: String::new(),
            ..Config::default()
        };
        assert!(config.validate().is_err());
    }
}
