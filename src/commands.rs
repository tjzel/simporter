use std::collections::{BTreeMap, HashSet};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Serialize;

use crate::cli::{
    ClaimArgs, DeviceAddArgs, DeviceForgetArgs, LeaseArgs, OutputArgs, ReleaseArgs, RenewArgs,
    StatusArgs, parse_duration, resolve_owner,
};
use crate::config::Config;
use crate::devices::{self, Device, Platform};
use crate::exit;
use crate::lock::LockGuard;
use crate::naming;
use crate::ports;
use crate::state::{Claim, ClaimedDevice, GcReport, Reservation, State};
use crate::{clock, config, state};

pub fn claim(config: &Config, args: &ClaimArgs) -> Result<i32> {
    let owner = resolve_owner(args.lease.owner.as_deref())?;
    let outcome = with_state(config, |state, now| {
        acquire(config, state, now, &args.lease, &owner, None)
    })?;
    Ok(report_lease(config, &outcome, args.output))
}

pub fn release(config: &Config, args: &ReleaseArgs) -> Result<i32> {
    let selector = release_selector(args)?;
    let (dropped, unreserved) = with_state(config, |state, _| {
        let mut dropped = Vec::new();
        state.claims.retain(|claim| {
            let matched = match &selector {
                Selector::Id(id) => claim.id == *id,
                Selector::Owner(owner) => claim.owner == *owner,
            };
            if matched {
                dropped.push(claim.id.clone());
            }
            !matched
        });
        let mut unreserved = Vec::new();
        if let Selector::Owner(owner) = &selector {
            state.reservations.retain(|reservation| {
                if reservation.owner == *owner {
                    unreserved.push(reservation.name.clone());
                    return false;
                }
                true
            });
        }
        Ok((dropped, unreserved))
    })?;
    if dropped.is_empty() && unreserved.is_empty() {
        bail!("no lease matched {selector}");
    }
    emit(
        args.output,
        &ReleasePayload {
            status: "released",
            released: &dropped,
            unreserved: &unreserved,
        },
        || {
            for id in &dropped {
                println!("released {id}");
            }
            for name in &unreserved {
                println!("dropped reservation {name}");
            }
        },
    );
    Ok(exit::OK)
}

pub fn renew(config: &Config, args: &RenewArgs) -> Result<i32> {
    let seconds = match args.ttl.as_deref() {
        Some(raw) => parse_duration(raw)?,
        None => config.default_ttl_secs,
    };
    let selector = renew_selector(args)?;
    let renewed = with_state(config, |state, now| {
        let mut renewed = Vec::new();
        for claim in &mut state.claims {
            let matched = match &selector {
                Selector::Id(id) => claim.id == *id,
                Selector::Owner(owner) => claim.owner == *owner,
            };
            if matched {
                claim.expires_at = now + seconds;
                renewed.push((claim.id.clone(), claim.expires_at));
            }
        }
        Ok(renewed)
    })?;
    if renewed.is_empty() {
        bail!("no lease matched {selector}");
    }
    let entries: Vec<RenewedEntry> = renewed
        .iter()
        .map(|(id, expires_at)| RenewedEntry {
            claim_id: id.clone(),
            expires_at: clock::rfc3339(*expires_at),
        })
        .collect();
    emit(
        args.output,
        &RenewPayload {
            status: "renewed",
            renewed: &entries,
        },
        || {
            for entry in &entries {
                println!("renewed {} until {}", entry.claim_id, entry.expires_at);
            }
        },
    );
    Ok(exit::OK)
}

pub fn status(config: &Config, args: &StatusArgs) -> Result<i32> {
    let now = clock::now();
    let state = with_state(config, |state, _| Ok(state.clone()))?;
    let discovered = devices::discover(config, args.platform).unwrap_or_default();
    let payload = build_status(config, &state, &discovered, now);
    emit(args.output, &payload, || print_status(&payload));
    Ok(exit::OK)
}

pub fn device_list(config: &Config, args: &StatusArgs) -> Result<i32> {
    let now = clock::now();
    let state = with_state(config, |state, _| Ok(state.clone()))?;
    let discovered = devices::discover(config, args.platform)?;
    let payload = build_status(config, &state, &discovered, now);
    emit(args.output, &payload.devices, || print_devices(&payload));
    Ok(exit::OK)
}

pub fn device_add(config: &Config, args: &DeviceAddArgs) -> Result<i32> {
    let owner = resolve_owner(args.lease.owner.as_deref())?;
    let outcome = with_state(config, |state, now| {
        let index = state
            .reservations
            .iter()
            .position(|reservation| reservation.name == args.name);
        if let Some(index) = index {
            let reservation = &state.reservations[index];
            if reservation.owner != owner {
                bail!(
                    "{:?} is reserved by {} until {}",
                    args.name,
                    reservation.owner,
                    clock::rfc3339(reservation.expires_at)
                );
            }
            state.reservations.remove(index);
        }
        acquire(config, state, now, &args.lease, &owner, Some(&args.name))
    })?;
    Ok(report_lease(config, &outcome, args.output))
}

pub fn device_forget(config: &Config, args: &DeviceForgetArgs) -> Result<i32> {
    let device = devices::find(config, &args.device)?;
    let dropped = with_state(config, |state, _| {
        let mut dropped = Vec::new();
        state.claims.retain(|claim| {
            let matched = claim
                .device
                .as_ref()
                .is_some_and(|claimed| claimed.id == device.id);
            if matched {
                dropped.push(claim.id.clone());
            }
            !matched
        });
        Ok(dropped)
    })?;
    emit(
        args.output,
        &ReleasePayload {
            status: "forgotten",
            released: &dropped,
            unreserved: &[],
        },
        || {
            if dropped.is_empty() {
                println!("{} was not claimed", device.name);
            }
            for id in &dropped {
                println!("dropped {id} holding {}", device.name);
            }
        },
    );
    Ok(exit::OK)
}

pub fn gc(config: &Config, args: OutputArgs) -> Result<i32> {
    let (state, report) = with_state_report(config, |state, _| Ok(state.clone()))?;
    let dropped: Vec<DroppedClaim> = report
        .dropped_claims
        .iter()
        .map(|(id, reason)| DroppedClaim {
            claim_id: id.clone(),
            reason: reason.to_string(),
        })
        .collect();
    let payload = GcPayload {
        status: "collected",
        dropped_claims: &dropped,
        dropped_reservations: &report.dropped_reservations,
        live_claims: state.claims.len(),
        live_reservations: state.reservations.len(),
    };
    emit(args, &payload, || {
        for entry in &dropped {
            println!("dropped {} ({})", entry.claim_id, entry.reason);
        }
        for name in &report.dropped_reservations {
            println!("dropped reservation {name}");
        }
        println!(
            "{} live leases, {} reservations",
            payload.live_claims, payload.live_reservations
        );
    });
    Ok(exit::OK)
}

pub fn doctor(config: &Config, args: OutputArgs) -> i32 {
    let mut checks = Vec::new();
    checks.push(check(
        "state directory",
        config::home().map(|path| path.display().to_string()),
    ));
    checks.push(check(
        "xcrun simctl",
        devices::ios::discover().map(|found| format!("{} devices", found.len())),
    ));
    checks.push(check(
        "android emulator",
        devices::android::discover().map(|found| format!("{} avds", found.len())),
    ));
    checks.push(check(
        "avdmanager",
        devices::android::tool("cmdline-tools/latest/bin/avdmanager"),
    ));
    checks.push(check(
        "port range",
        ports::allocate(config, &State::default(), &["probe".to_string()]).map(|found| {
            format!(
                "{} available from {}",
                found[0].port, config.port_range_start
            )
        }),
    ));
    let ok = checks.iter().all(|entry| entry.ok);
    emit(
        args,
        &DoctorPayload {
            status: if ok { "ok" } else { "degraded" },
            checks: &checks,
        },
        || {
            for entry in &checks {
                let mark = if entry.ok { "ok " } else { "!! " };
                println!("{mark}{:<18} {}", entry.name, entry.detail);
            }
        },
    );
    exit::OK
}

pub enum LeaseOutcome {
    Claimed {
        claim: Claim,
        warnings: Vec<String>,
    },
    PoolExhausted {
        platform: Platform,
        name: String,
        device_type: Option<String>,
        runtime: Option<String>,
        expires_at: u64,
    },
}

pub fn acquire(
    config: &Config,
    state: &mut State,
    now: u64,
    lease: &LeaseArgs,
    owner: &str,
    forced_device: Option<&str>,
) -> Result<LeaseOutcome> {
    let device = if lease.no_device {
        None
    } else {
        match select_device(config, state, lease, forced_device)? {
            Selection::Device(device) => Some(device),
            Selection::Exhausted(pool) => {
                let name = naming::next_free_name(
                    config,
                    lease.platform,
                    &lease
                        .device_type
                        .clone()
                        .unwrap_or_else(|| naming::default_base(config, lease.platform)),
                    &pool,
                );
                let expires_at = now + config.reservation_ttl_secs;
                state.reservations.push(Reservation {
                    name: name.clone(),
                    owner: owner.to_string(),
                    platform: lease.platform,
                    expires_at,
                });
                return Ok(LeaseOutcome::PoolExhausted {
                    platform: lease.platform,
                    name,
                    device_type: lease.device_type.clone(),
                    runtime: lease.runtime.clone(),
                    expires_at,
                });
            }
        }
    };

    let names = ports::normalize_names(&lease.ports, lease.n_ports)?;
    let warnings = collides_with_own_ports(state, owner, &names);
    let allocated = ports::allocate(config, state, &names)?;
    let ttl = match lease.ttl.as_deref() {
        Some(raw) => parse_duration(raw)?,
        None => config.default_ttl_secs,
    };
    let claim = Claim {
        id: state.next_claim_id(now),
        owner: owner.to_string(),
        owner_pid: lease.owner_pid,
        label: lease.label.clone(),
        device: device.map(|device| ClaimedDevice {
            id: device.id,
            name: device.name,
            platform: device.platform,
            serial: device.serial,
        }),
        ports: allocated,
        created_at: now,
        expires_at: now + ttl,
    };
    state.claims.push(claim.clone());
    Ok(LeaseOutcome::Claimed { claim, warnings })
}

fn collides_with_own_ports(state: &State, owner: &str, names: &[String]) -> Vec<String> {
    let mut warnings = Vec::new();
    for claim in state.claims.iter().filter(|claim| claim.owner == owner) {
        for held in &claim.ports {
            if names
                .iter()
                .any(|name| name.eq_ignore_ascii_case(&held.name))
            {
                warnings.push(format!(
                    "you already hold a port named {:?} on claim {}; both leases export {} and the \
                     second value wins, so give this one a distinct name",
                    held.name,
                    claim.id,
                    ports::env_key(&held.name)
                ));
            }
        }
    }
    warnings
}

pub fn with_state<T>(
    config: &Config,
    body: impl FnOnce(&mut State, u64) -> Result<T>,
) -> Result<T> {
    Ok(with_state_report(config, body)?.0)
}

fn with_state_report<T>(
    config: &Config,
    body: impl FnOnce(&mut State, u64) -> Result<T>,
) -> Result<(T, GcReport)> {
    let lock_path = config::lock_path()?;
    let timeout = Duration::from_secs(config.lock_timeout_secs);
    let Some(_guard) = LockGuard::acquire(&lock_path, timeout)? else {
        return Err(anyhow::Error::new(exit::LockTimeout {
            path: lock_path,
            seconds: config.lock_timeout_secs,
        }));
    };
    let state_path = config::state_path()?;
    let mut current = state::load(&state_path)?;
    let now = clock::now();
    let report = current.gc(now);
    let outcome = body(&mut current, now)?;
    state::save(&state_path, &current)?;
    Ok((outcome, report))
}

enum Selection {
    Device(Device),
    Exhausted(HashSet<String>),
}

fn select_device(
    config: &Config,
    state: &State,
    lease: &LeaseArgs,
    forced: Option<&str>,
) -> Result<Selection> {
    let wanted = forced.or(lease.device.as_deref());
    let discovered = devices::discover(config, Some(lease.platform))?;
    if let Some(wanted) = wanted {
        let device = discovered
            .into_iter()
            .find(|device| device.id == wanted || device.name == wanted)
            .with_context(|| format!("no {} device matches {wanted:?}", lease.platform))?;
        if !device.managed {
            bail!(
                "{:?} is not part of the agent pool; only devices named like {:?} are claimable, \
                 so devices you use yourself are never taken",
                device.name,
                naming::format_name(
                    config,
                    lease.platform,
                    &naming::default_base(config, lease.platform),
                    1
                )
            );
        }
        if let Some(holder) = state.holder_of_device(&device.id) {
            bail!(
                "{:?} is already claimed by {} until {}",
                device.name,
                holder.owner,
                clock::rfc3339(holder.expires_at)
            );
        }
        return Ok(Selection::Device(device));
    }

    let mut free: Vec<Device> = discovered
        .iter()
        .filter(|device| device.managed && state.holder_of_device(&device.id).is_none())
        .cloned()
        .collect();
    free.sort_by_key(|device| (!device.running, device.name.clone()));
    if let Some(device) = free.into_iter().next() {
        return Ok(Selection::Device(device));
    }
    let mut pool: HashSet<String> = discovered.into_iter().map(|device| device.name).collect();
    pool.extend(state.reserved_names().into_iter().map(String::from));
    Ok(Selection::Exhausted(pool))
}

pub fn report_lease(config: &Config, outcome: &LeaseOutcome, output: OutputArgs) -> i32 {
    match outcome {
        LeaseOutcome::Claimed { claim, warnings } => {
            for warning in warnings {
                eprintln!("simporter: {warning}");
            }
            let payload = claim_payload(claim, warnings);
            emit(output, &payload, || print_claim(&payload));
            exit::OK
        }
        LeaseOutcome::PoolExhausted {
            platform,
            name,
            device_type,
            runtime,
            expires_at,
        } => {
            let command = devices::create_plan(
                config,
                *platform,
                name,
                device_type.as_deref(),
                runtime.as_deref(),
            )
            .unwrap_or_else(|error| format!("# cannot build a create command: {error:#}"));
            let next_step = format!("{command}\nsimporter device add '{name}'");
            let hint = format!(
                "every pooled device carries the {:?} suffix so your own devices stay untouched",
                config.agent_suffix
            );
            let payload = ExhaustedPayload {
                status: "pool_exhausted",
                platform: *platform,
                reserved_name: name,
                reservation_expires_at: clock::rfc3339(*expires_at),
                create_command: &command,
                next_step: &next_step,
                hint: &hint,
            };
            emit(output, &payload, || {
                println!("no free {platform} device in the agent pool");
                println!("reserved name  {name}");
                println!("create it      {command}");
                println!("then claim it  simporter device add '{name}'");
            });
            exit::POOL_EXHAUSTED
        }
    }
}

fn build_status(config: &Config, state: &State, discovered: &[Device], now: u64) -> StatusPayload {
    let claims = state
        .claims
        .iter()
        .map(|claim| ClaimPayload {
            claim_id: claim.id.clone(),
            owner: claim.owner.clone(),
            owner_pid: claim.owner_pid,
            label: claim.label.clone(),
            device: claim.device.as_ref().map(|device| DevicePayload {
                id: device.id.clone(),
                name: device.name.clone(),
                platform: device.platform,
                serial: device.serial.clone(),
                running: discovered
                    .iter()
                    .find(|found| found.id == device.id)
                    .is_some_and(|found| found.running),
            }),
            ports: claim
                .ports
                .iter()
                .map(|port| (port.name.clone(), port.port))
                .collect(),
            env: environment(claim),
            expires_at: clock::rfc3339(claim.expires_at),
            expires_in: clock::humanize(claim.expires_at.saturating_sub(now)),
            warnings: Vec::new(),
        })
        .collect();
    let devices = discovered
        .iter()
        .map(|device| PoolEntry {
            id: device.id.clone(),
            name: device.name.clone(),
            platform: device.platform,
            running: device.running,
            pool: if device.managed { "agent" } else { "unmanaged" },
            held_by: state
                .holder_of_device(&device.id)
                .map(|claim| claim.owner.clone()),
        })
        .collect();
    let reservations = state
        .reservations
        .iter()
        .map(|reservation| ReservationPayload {
            name: reservation.name.clone(),
            owner: reservation.owner.clone(),
            platform: reservation.platform,
            expires_at: clock::rfc3339(reservation.expires_at),
        })
        .collect();
    StatusPayload {
        claims,
        devices,
        reservations,
        port_range: format!("{}-{}", config.port_range_start, config.port_range_end),
    }
}

pub fn claim_payload(claim: &Claim, warnings: &[String]) -> ClaimPayload {
    ClaimPayload {
        claim_id: claim.id.clone(),
        owner: claim.owner.clone(),
        owner_pid: claim.owner_pid,
        label: claim.label.clone(),
        device: claim.device.as_ref().map(|device| DevicePayload {
            id: device.id.clone(),
            name: device.name.clone(),
            platform: device.platform,
            serial: device.serial.clone(),
            running: false,
        }),
        ports: claim
            .ports
            .iter()
            .map(|port| (port.name.clone(), port.port))
            .collect(),
        env: environment(claim),
        expires_at: clock::rfc3339(claim.expires_at),
        expires_in: clock::humanize(claim.expires_at.saturating_sub(clock::now())),
        warnings: warnings.to_vec(),
    }
}

pub fn environment(claim: &Claim) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    env.insert("SIMPORTER_CLAIM_ID".to_string(), claim.id.clone());
    env.insert("SIMPORTER_OWNER".to_string(), claim.owner.clone());
    if let Some(device) = &claim.device {
        env.insert(
            "SIMPORTER_DEVICE".to_string(),
            device.serial.clone().unwrap_or_else(|| device.id.clone()),
        );
        env.insert("SIMPORTER_DEVICE_ID".to_string(), device.id.clone());
        env.insert("SIMPORTER_DEVICE_NAME".to_string(), device.name.clone());
        env.insert(
            "SIMPORTER_DEVICE_PLATFORM".to_string(),
            device.platform.to_string(),
        );
    }
    for port in &claim.ports {
        env.insert(ports::env_key(&port.name), port.port.to_string());
    }
    env
}

fn print_claim(payload: &ClaimPayload) {
    println!("claim   {}", payload.claim_id);
    println!("owner   {}", payload.owner);
    if let Some(device) = &payload.device {
        println!(
            "device  {} [{}] {}",
            device.name, device.id, device.platform
        );
    }
    for (name, port) in &payload.ports {
        println!("port    {name:<12} {port}");
    }
    println!("expires {} (in {})", payload.expires_at, payload.expires_in);
}

fn print_status(payload: &StatusPayload) {
    println!("ports {}", payload.port_range);
    if payload.claims.is_empty() {
        println!("no active leases");
    }
    for claim in &payload.claims {
        let device = claim
            .device
            .as_ref()
            .map_or_else(|| "-".to_string(), |device| device.name.clone());
        let ports: Vec<String> = claim
            .ports
            .iter()
            .map(|(name, port)| format!("{name}={port}"))
            .collect();
        println!(
            "lease {} owner={} device={} ports=[{}] expires_in={}",
            claim.claim_id,
            claim.owner,
            device,
            ports.join(" "),
            claim.expires_in
        );
    }
    print_devices(payload);
    for reservation in &payload.reservations {
        println!(
            "reserved {} for {} until {}",
            reservation.name, reservation.owner, reservation.expires_at
        );
    }
}

fn print_devices(payload: &StatusPayload) {
    for device in &payload.devices {
        let held = device.held_by.as_deref().unwrap_or("-");
        let running = if device.running { "running" } else { "stopped" };
        println!(
            "device {:<9} {:<9} {:<7} held_by={:<12} {}",
            device.pool, device.platform, running, held, device.name
        );
    }
}

fn check(name: &'static str, outcome: Result<String>) -> Check {
    match outcome {
        Ok(detail) => Check {
            name,
            ok: true,
            detail,
        },
        Err(error) => Check {
            name,
            ok: false,
            detail: format!("{error:#}"),
        },
    }
}

fn emit<T: Serialize>(output: OutputArgs, payload: &T, text: impl FnOnce()) {
    if output.json {
        match serde_json::to_string_pretty(payload) {
            Ok(body) => println!("{body}"),
            Err(error) => eprintln!("failed to encode json: {error}"),
        }
        return;
    }
    text();
}

enum Selector {
    Id(String),
    Owner(String),
}

impl std::fmt::Display for Selector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Id(id) => write!(f, "claim {id}"),
            Self::Owner(owner) => write!(f, "owner {owner}"),
        }
    }
}

fn release_selector(args: &ReleaseArgs) -> Result<Selector> {
    match (&args.claim_id, &args.owner, args.all) {
        (Some(id), None, false) => Ok(Selector::Id(id.clone())),
        (None, Some(owner), true) => Ok(Selector::Owner(owner.clone())),
        (None, None, true) => Ok(Selector::Owner(resolve_owner(None)?)),
        _ => bail!("pass a claim id, or --owner <id> --all"),
    }
}

fn renew_selector(args: &RenewArgs) -> Result<Selector> {
    match (&args.claim_id, &args.owner) {
        (Some(id), None) => Ok(Selector::Id(id.clone())),
        (None, Some(owner)) => Ok(Selector::Owner(owner.clone())),
        (None, None) => Ok(Selector::Owner(resolve_owner(None)?)),
        _ => bail!("pass a claim id or --owner <id>, not both"),
    }
}

#[derive(Debug, Serialize)]
pub struct ClaimPayload {
    pub claim_id: String,
    pub owner: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device: Option<DevicePayload>,
    pub ports: BTreeMap<String, u16>,
    pub env: BTreeMap<String, String>,
    pub expires_at: String,
    pub expires_in: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct DevicePayload {
    pub id: String,
    pub name: String,
    pub platform: Platform,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub serial: Option<String>,
    pub running: bool,
}

#[derive(Debug, Serialize)]
struct StatusPayload {
    claims: Vec<ClaimPayload>,
    devices: Vec<PoolEntry>,
    reservations: Vec<ReservationPayload>,
    port_range: String,
}

#[derive(Debug, Serialize)]
struct PoolEntry {
    id: String,
    name: String,
    platform: Platform,
    running: bool,
    pool: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    held_by: Option<String>,
}

#[derive(Debug, Serialize)]
struct ReservationPayload {
    name: String,
    owner: String,
    platform: Platform,
    expires_at: String,
}

#[derive(Debug, Serialize)]
struct ExhaustedPayload<'a> {
    status: &'static str,
    platform: Platform,
    reserved_name: &'a str,
    reservation_expires_at: String,
    create_command: &'a str,
    next_step: &'a str,
    hint: &'a str,
}

#[derive(Debug, Serialize)]
struct ReleasePayload<'a> {
    status: &'static str,
    released: &'a [String],
    #[serde(skip_serializing_if = "<[String]>::is_empty")]
    unreserved: &'a [String],
}

#[derive(Debug, Serialize)]
struct RenewPayload<'a> {
    status: &'static str,
    renewed: &'a [RenewedEntry],
}

#[derive(Debug, Serialize)]
struct RenewedEntry {
    claim_id: String,
    expires_at: String,
}

#[derive(Debug, Serialize)]
struct GcPayload<'a> {
    status: &'static str,
    dropped_claims: &'a [DroppedClaim],
    dropped_reservations: &'a [String],
    live_claims: usize,
    live_reservations: usize,
}

#[derive(Debug, Serialize)]
struct DroppedClaim {
    claim_id: String,
    reason: String,
}

#[derive(Debug, Serialize)]
struct DoctorPayload<'a> {
    status: &'static str,
    checks: &'a [Check],
}

#[derive(Debug, Serialize)]
struct Check {
    name: &'static str,
    ok: bool,
    detail: String,
}
