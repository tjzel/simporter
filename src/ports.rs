use std::collections::HashSet;
use std::io::ErrorKind;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener};

use anyhow::{Result, bail};

use crate::config::Config;
use crate::exit::NoFreePort;
use crate::state::{ClaimedPort, State};

pub fn allocate(config: &Config, state: &State, names: &[String]) -> Result<Vec<ClaimedPort>> {
    let mut taken = state.claimed_ports();
    let mut allocated = Vec::with_capacity(names.len());
    for name in names {
        let port = pick(config, &taken)?;
        taken.insert(port);
        allocated.push(ClaimedPort {
            name: name.clone(),
            port,
        });
    }
    Ok(allocated)
}

pub fn normalize_names(named: &[String], count: Option<u16>) -> Result<Vec<String>> {
    let mut names: Vec<String> = Vec::new();
    for raw in named {
        for part in raw.split(',') {
            let name = part.trim();
            if name.is_empty() {
                continue;
            }
            validate_name(name)?;
            names.push(name.to_string());
        }
    }
    if let Some(count) = count {
        let offset = names.len();
        for index in 0..count {
            names.push((offset + index as usize + 1).to_string());
        }
    }
    let mut seen = HashSet::new();
    for name in &names {
        if !seen.insert(name.to_lowercase()) {
            bail!("duplicate port name {name:?}");
        }
    }
    Ok(names)
}

pub fn env_key(name: &str) -> String {
    let body: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("SIMPORTER_PORT_{body}")
}

pub fn is_free(port: u16) -> bool {
    let probes = [
        probe(SocketAddr::from((Ipv4Addr::LOCALHOST, port))),
        probe(SocketAddr::from((Ipv4Addr::UNSPECIFIED, port))),
        probe(SocketAddr::from((Ipv6Addr::UNSPECIFIED, port))),
    ];
    probes.iter().all(|outcome| *outcome != Probe::InUse)
        && probes[0] == Probe::Free
        && probes[1] == Probe::Free
}

fn pick(config: &Config, taken: &HashSet<u16>) -> Result<u16> {
    for port in config.port_range_start..=config.port_range_end {
        if taken.contains(&port) {
            continue;
        }
        if is_free(port) {
            return Ok(port);
        }
    }
    Err(anyhow::Error::new(NoFreePort {
        message: format!(
            "no free port left in {}-{} ({} already claimed, the rest are held by other processes)",
            config.port_range_start,
            config.port_range_end,
            taken.len()
        ),
    }))
}

fn probe(address: SocketAddr) -> Probe {
    match TcpListener::bind(address) {
        Ok(_) => Probe::Free,
        Err(error) if error.kind() == ErrorKind::AddrInUse => Probe::InUse,
        Err(_) => Probe::Unavailable,
    }
}

fn validate_name(name: &str) -> Result<()> {
    let usable = name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !usable || name.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        bail!(
            "port name {name:?} must be alphanumeric, may contain - or _, and cannot start with a digit"
        );
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Probe {
    Free,
    InUse,
    Unavailable,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Config {
        Config {
            port_range_start: 18081,
            port_range_end: 18090,
            ..Config::default()
        }
    }

    #[test]
    fn allocation_skips_ports_already_in_the_registry() {
        let mut state = State::default();
        state.claims.push(crate::state::Claim {
            id: "held".to_string(),
            owner: "other".to_string(),
            owner_pid: None,
            label: None,
            device: None,
            ports: vec![ClaimedPort {
                name: "metro".to_string(),
                port: 18081,
            }],
            created_at: 0,
            expires_at: u64::MAX,
        });
        let allocated = allocate(&config(), &state, &["metro".to_string()]).unwrap();
        assert_ne!(allocated[0].port, 18081);
    }

    #[test]
    fn allocation_returns_distinct_ports_within_one_call() {
        let names = vec![
            "metro".to_string(),
            "inspector".to_string(),
            "extra".to_string(),
        ];
        let allocated = allocate(&config(), &State::default(), &names).unwrap();
        let unique: HashSet<u16> = allocated.iter().map(|port| port.port).collect();
        assert_eq!(unique.len(), 3);
    }

    #[test]
    fn a_port_held_by_a_live_listener_is_not_free() {
        let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(!is_free(port));
    }

    #[test]
    fn a_loopback_only_listener_is_still_detected() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(!is_free(port));
    }

    #[test]
    fn allocation_refuses_a_port_a_foreign_process_listens_on() {
        let narrow = Config {
            port_range_start: 18101,
            port_range_end: 18102,
            ..Config::default()
        };
        let blocker = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 18101)).unwrap();
        let allocated = allocate(&narrow, &State::default(), &["metro".to_string()]).unwrap();
        assert_eq!(allocated[0].port, 18102);
        drop(blocker);
    }

    #[test]
    fn allocation_fails_when_the_range_is_exhausted() {
        let narrow = Config {
            port_range_start: 18091,
            port_range_end: 18091,
            ..Config::default()
        };
        let mut state = State::default();
        state.claims.push(crate::state::Claim {
            id: "held".to_string(),
            owner: "other".to_string(),
            owner_pid: None,
            label: None,
            device: None,
            ports: vec![ClaimedPort {
                name: "metro".to_string(),
                port: 18091,
            }],
            created_at: 0,
            expires_at: u64::MAX,
        });
        let error = allocate(&narrow, &state, &["metro".to_string()]).unwrap_err();
        assert!(error.to_string().contains("no free port left"));
    }

    #[test]
    fn names_are_split_and_counted() {
        let names = normalize_names(&["metro,inspector".to_string()], Some(2)).unwrap();
        assert_eq!(names, vec!["metro", "inspector", "3", "4"]);
    }

    #[test]
    fn duplicate_names_are_rejected() {
        assert!(normalize_names(&["metro,metro".to_string()], None).is_err());
    }

    #[test]
    fn invalid_names_are_rejected() {
        assert!(normalize_names(&["metro server".to_string()], None).is_err());
        assert!(normalize_names(&["9lives".to_string()], None).is_err());
    }

    #[test]
    fn env_keys_are_uppercase_and_underscored() {
        assert_eq!(env_key("metro"), "SIMPORTER_PORT_METRO");
        assert_eq!(env_key("dev-tools"), "SIMPORTER_PORT_DEV_TOOLS");
        assert_eq!(env_key("1"), "SIMPORTER_PORT_1");
    }
}
