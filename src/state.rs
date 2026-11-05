use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::devices::Platform;
use crate::lock::process_is_alive;

const STATE_VERSION: u32 = 1;
const MAX_LINK_HOPS: u8 = 16;

pub fn load(path: &Path) -> Result<State> {
    if !path.exists() {
        return Ok(State::default());
    }
    let raw = fs::read_to_string(path)
        .with_context(|| format!("failed to read state at {}", path.display()))?;
    if raw.trim().is_empty() {
        return Ok(State::default());
    }
    serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse state at {}", path.display()))
}

pub fn save(path: &Path, state: &State) -> Result<()> {
    let body = serde_json::to_string_pretty(state)?;
    write_atomic(path, &body)
}

pub fn write_atomic(path: &Path, body: &str) -> Result<()> {
    let target = resolve_links(path);
    let parent = target
        .parent()
        .context("state path has no parent directory")?;
    let temp = parent.join(format!(
        ".{}.{}.tmp",
        file_name(&target),
        std::process::id()
    ));
    fs::write(&temp, body)
        .with_context(|| format!("failed to write temp file {}", temp.display()))?;
    fs::rename(&temp, &target)
        .with_context(|| format!("failed to move {} into place", temp.display()))?;
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct State {
    pub version: u32,
    #[serde(default)]
    pub claims: Vec<Claim>,
    #[serde(default)]
    pub reservations: Vec<Reservation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub heavy: Vec<HeavyJob>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            claims: Vec::new(),
            reservations: Vec::new(),
            heavy: Vec::new(),
        }
    }
}

impl State {
    pub fn gc(&mut self, now: u64) -> GcReport {
        let mut report = GcReport::default();
        self.claims.retain(|claim| match claim.death_reason(now) {
            Some(reason) => {
                report.dropped_claims.push((claim.id.clone(), reason));
                false
            }
            None => true,
        });
        self.reservations.retain(|reservation| {
            if reservation.expires_at <= now {
                report.dropped_reservations.push(reservation.name.clone());
                return false;
            }
            true
        });
        self.heavy.retain(|job| {
            if process_is_alive(job.pid) {
                return true;
            }
            report.dropped_heavy.push(job.id.clone());
            false
        });
        report
    }

    pub fn holder_of_device(&self, device_id: &str) -> Option<&Claim> {
        self.claims.iter().find(|claim| {
            claim
                .device
                .as_ref()
                .is_some_and(|device| device.id == device_id)
        })
    }

    pub fn claimed_ports(&self) -> HashSet<u16> {
        self.claims
            .iter()
            .flat_map(|claim| claim.ports.iter().map(|port| port.port))
            .collect()
    }

    pub fn reserved_names(&self) -> HashSet<&str> {
        self.reservations
            .iter()
            .map(|reservation| reservation.name.as_str())
            .collect()
    }

    pub fn heavy_in_use(&self) -> u32 {
        self.heavy
            .iter()
            .filter(|job| job.started_at.is_some())
            .map(|job| job.weight)
            .sum()
    }

    pub fn heavy_can_start(&self, id: &str, slots: u32) -> bool {
        let Some(first) = self.heavy.iter().find(|job| job.started_at.is_none()) else {
            return false;
        };
        first.id == id && self.heavy_in_use() + first.weight <= slots
    }

    pub fn next_heavy_id(&self, now: u64) -> String {
        let taken: HashSet<&str> = self.heavy.iter().map(|job| job.id.as_str()).collect();
        for seq in 0..u32::MAX {
            let candidate = format!("h{now:x}-{:x}-{seq:x}", std::process::id());
            if !taken.contains(candidate.as_str()) {
                return candidate;
            }
        }
        unreachable!("heavy job id space exhausted")
    }

    pub fn next_claim_id(&self, now: u64) -> String {
        let taken: HashSet<&str> = self.claims.iter().map(|claim| claim.id.as_str()).collect();
        for seq in 0..u32::MAX {
            let candidate = format!("c{now:x}-{:x}-{seq:x}", std::process::id());
            if !taken.contains(candidate.as_str()) {
                return candidate;
            }
        }
        unreachable!("claim id space exhausted")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claim {
    pub id: String,
    pub owner: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<ClaimedDevice>,
    #[serde(default)]
    pub ports: Vec<ClaimedPort>,
    pub created_at: u64,
    pub expires_at: u64,
}

impl Claim {
    pub fn death_reason(&self, now: u64) -> Option<DeathReason> {
        if let Some(pid) = self.owner_pid
            && !process_is_alive(pid)
        {
            return Some(DeathReason::OwnerGone);
        }
        if self.expires_at <= now {
            return Some(DeathReason::Expired);
        }
        None
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimedDevice {
    pub id: String,
    pub name: String,
    pub platform: Platform,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serial: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimedPort {
    pub name: String,
    pub port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reservation {
    pub name: String,
    pub owner: String,
    pub platform: Platform,
    pub expires_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeavyJob {
    pub id: String,
    pub owner: String,
    pub label: String,
    pub weight: u32,
    pub pid: u32,
    pub queued_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeathReason {
    Expired,
    OwnerGone,
}

impl std::fmt::Display for DeathReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Expired => write!(f, "lease expired"),
            Self::OwnerGone => write!(f, "owner process gone"),
        }
    }
}

#[derive(Debug, Default)]
pub struct GcReport {
    pub dropped_claims: Vec<(String, DeathReason)>,
    pub dropped_reservations: Vec<String>,
    pub dropped_heavy: Vec<String>,
}

pub fn resolve_links(path: &Path) -> PathBuf {
    let mut current = path.to_path_buf();
    for _ in 0..MAX_LINK_HOPS {
        let Ok(meta) = fs::symlink_metadata(&current) else {
            return current;
        };
        if !meta.file_type().is_symlink() {
            return current;
        }
        let Ok(next) = fs::read_link(&current) else {
            return current;
        };
        current = if next.is_absolute() {
            next
        } else {
            match current.parent() {
                Some(parent) => parent.join(next),
                None => next,
            }
        };
    }
    current
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || "state".to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claim_at(id: &str, expires_at: u64, owner_pid: Option<u32>) -> Claim {
        Claim {
            id: id.to_string(),
            owner: "owner".to_string(),
            owner_pid,
            label: None,
            device: None,
            ports: vec![ClaimedPort {
                name: "metro".to_string(),
                port: 8081,
            }],
            created_at: 0,
            expires_at,
        }
    }

    #[test]
    fn gc_drops_expired_claims_and_keeps_live_ones() {
        let mut state = State {
            claims: vec![claim_at("old", 100, None), claim_at("live", 500, None)],
            ..State::default()
        };
        let report = state.gc(200);
        assert_eq!(state.claims.len(), 1);
        assert_eq!(state.claims[0].id, "live");
        assert_eq!(
            report.dropped_claims,
            vec![("old".to_string(), DeathReason::Expired)]
        );
    }

    #[test]
    fn gc_drops_claims_whose_owner_process_is_gone() {
        let mut state = State {
            claims: vec![
                claim_at("dead", u64::MAX, Some(u32::MAX)),
                claim_at("alive", u64::MAX, Some(std::process::id())),
            ],
            ..State::default()
        };
        let report = state.gc(0);
        assert_eq!(state.claims.len(), 1);
        assert_eq!(state.claims[0].id, "alive");
        assert_eq!(
            report.dropped_claims,
            vec![("dead".to_string(), DeathReason::OwnerGone)]
        );
    }

    #[test]
    fn gc_expires_stale_reservations() {
        let mut state = State {
            reservations: vec![
                Reservation {
                    name: "iPhone 17 Pro Agent 1".to_string(),
                    owner: "a".to_string(),
                    platform: Platform::Ios,
                    expires_at: 100,
                },
                Reservation {
                    name: "iPhone 17 Pro Agent 2".to_string(),
                    owner: "b".to_string(),
                    platform: Platform::Ios,
                    expires_at: 900,
                },
            ],
            ..State::default()
        };
        state.gc(500);
        assert_eq!(state.reservations.len(), 1);
        assert_eq!(state.reservations[0].name, "iPhone 17 Pro Agent 2");
    }

    #[test]
    fn claimed_ports_collects_every_claim() {
        let mut second = claim_at("b", u64::MAX, None);
        second.ports = vec![ClaimedPort {
            name: "metro".to_string(),
            port: 8082,
        }];
        let state = State {
            claims: vec![claim_at("a", u64::MAX, None), second],
            ..State::default()
        };
        let ports = state.claimed_ports();
        assert!(ports.contains(&8081) && ports.contains(&8082));
        assert_eq!(ports.len(), 2);
    }

    #[test]
    fn round_trip_through_disk_preserves_claims() {
        let dir = crate::testing::temp_dir("state-round-trip");
        let path = dir.join("state.json");
        let state = State {
            claims: vec![claim_at("a", 42, None)],
            ..State::default()
        };
        save(&path, &state).unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.claims.len(), 1);
        assert_eq!(loaded.claims[0].ports[0].port, 8081);
    }

    #[test]
    fn writing_through_a_symlink_keeps_the_link_and_updates_the_target() {
        let dir = crate::testing::temp_dir("state-symlink");
        let target = dir.join("canonical.md");
        let alias = dir.join("alias.md");
        fs::write(&target, "old").unwrap();
        std::os::unix::fs::symlink(&target, &alias).unwrap();

        write_atomic(&alias, "new").unwrap();

        assert!(
            fs::symlink_metadata(&alias)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_to_string(&target).unwrap(), "new");
    }

    #[test]
    fn a_chain_of_symlinks_resolves_to_the_final_target() {
        let dir = crate::testing::temp_dir("state-symlink-chain");
        let target = dir.join("canonical.md");
        let middle = dir.join("middle.md");
        let alias = dir.join("alias.md");
        fs::write(&target, "old").unwrap();
        std::os::unix::fs::symlink(&target, &middle).unwrap();
        std::os::unix::fs::symlink(&middle, &alias).unwrap();

        write_atomic(&alias, "new").unwrap();

        assert!(
            fs::symlink_metadata(&middle)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_to_string(&target).unwrap(), "new");
    }

    #[test]
    fn a_plain_path_is_returned_unchanged() {
        let dir = crate::testing::temp_dir("state-no-symlink");
        let path = dir.join("plain.json");
        assert_eq!(resolve_links(&path), path);
    }

    #[test]
    fn missing_state_file_loads_as_empty() {
        let dir = crate::testing::temp_dir("state-missing");
        let loaded = load(&dir.join("state.json")).unwrap();
        assert!(loaded.claims.is_empty());
    }
}
