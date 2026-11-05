use anyhow::{Result, bail};
use clap::{Args, Parser, Subcommand};

use crate::devices::Platform;

#[derive(Debug, Parser)]
#[command(
    name = "simporter",
    version,
    about = "Claim a simulator and TCP ports so parallel agents never collide"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Take an exclusive lease on one device and any ports you need
    Claim(ClaimArgs),
    /// Give a lease back
    Release(ReleaseArgs),
    /// Push a lease expiry further out
    Renew(RenewArgs),
    /// Show every lease, device and port this machine knows about
    Status(StatusArgs),
    /// Inspect or extend the agent device pool
    #[command(subcommand)]
    Device(DeviceCommand),
    /// Hold a lease for exactly as long as a child command runs
    Run(RunArgs),
    /// Wait for a heavy-work slot, then run a CPU-heavy command while holding it
    Heavy(HeavyArgs),
    /// Hook for Claude Code: refuse heavy commands not run through `simporter heavy`
    Hook,
    /// Drop leases whose owner is gone or whose lease expired
    Gc(OutputArgs),
    /// Check that the tooling this depends on is present
    Doctor(OutputArgs),
    /// Print the rule that tells every agent to use simporter, or install it with its skill
    Rule(RuleArgs),
}

#[derive(Debug, Args)]
pub struct RuleArgs {
    /// Write the rule into every installed agent's instruction file and install the skill
    #[arg(long)]
    pub install: bool,
    /// Print the skill with the full guide instead of the rule
    #[arg(long, conflicts_with_all = ["install", "dry_run"])]
    pub skill: bool,
    /// Show what --install would change, without writing
    #[arg(long)]
    pub dry_run: bool,
    /// Manage exactly this rule file instead of auto-detecting harnesses, repeatable
    #[arg(long, value_name = "PATH")]
    pub target: Vec<std::path::PathBuf>,
    /// Install the skill into this skills directory instead of auto-detecting, repeatable
    #[arg(long, value_name = "DIR")]
    pub skill_target: Vec<std::path::PathBuf>,
    #[command(flatten)]
    pub output: OutputArgs,
}

#[derive(Debug, Args)]
pub struct ClaimArgs {
    #[command(flatten)]
    pub lease: LeaseArgs,
    #[command(flatten)]
    pub output: OutputArgs,
}

#[derive(Debug, Args, Clone)]
pub struct LeaseArgs {
    /// Which device family to claim from
    #[arg(long, value_enum, default_value_t = Platform::Ios)]
    pub platform: Platform,
    /// Claim this exact device by UDID, AVD name or simulator name
    #[arg(long)]
    pub device: Option<String>,
    /// Claim ports only, no device
    #[arg(long, conflicts_with = "device")]
    pub no_device: bool,
    /// Named ports to reserve, repeatable and comma separated
    #[arg(long, value_name = "NAME[,NAME...]")]
    pub ports: Vec<String>,
    /// Reserve this many extra unnamed ports
    #[arg(long, value_name = "N")]
    pub n_ports: Option<u16>,
    /// Lease length, for example 45s, 30m or 2h
    #[arg(long, value_name = "DURATION")]
    pub ttl: Option<String>,
    /// Who owns the lease; defaults to the `SIMPORTER_OWNER` variable
    #[arg(long)]
    pub owner: Option<String>,
    /// Release the lease automatically when this process dies
    #[arg(long, value_name = "PID")]
    pub owner_pid: Option<u32>,
    /// Free text shown in status
    #[arg(long)]
    pub label: Option<String>,
    /// Device type to suggest when the pool is empty
    #[arg(long, value_name = "NAME")]
    pub device_type: Option<String>,
    /// Runtime or system image to suggest when the pool is empty
    #[arg(long, value_name = "ID")]
    pub runtime: Option<String>,
}

#[derive(Debug, Args)]
pub struct ReleaseArgs {
    /// Lease to release
    pub claim_id: Option<String>,
    /// Release every lease held by this owner
    #[arg(long)]
    pub owner: Option<String>,
    /// Required together with --owner
    #[arg(long)]
    pub all: bool,
    #[command(flatten)]
    pub output: OutputArgs,
}

#[derive(Debug, Args)]
pub struct RenewArgs {
    /// Lease to renew
    pub claim_id: Option<String>,
    /// Renew every lease held by this owner
    #[arg(long)]
    pub owner: Option<String>,
    /// New lease length counted from now
    #[arg(long, value_name = "DURATION")]
    pub ttl: Option<String>,
    #[command(flatten)]
    pub output: OutputArgs,
}

#[derive(Debug, Args)]
pub struct StatusArgs {
    /// Limit the report to one device family
    #[arg(long, value_enum)]
    pub platform: Option<Platform>,
    #[command(flatten)]
    pub output: OutputArgs,
}

#[derive(Debug, Subcommand)]
pub enum DeviceCommand {
    /// List the agent pool and every device excluded from it
    List(StatusArgs),
    /// Register a device you just created, then claim it in the same step
    Add(DeviceAddArgs),
    /// Force a device back into the pool by dropping the lease on it
    Forget(DeviceForgetArgs),
}

#[derive(Debug, Args)]
pub struct DeviceAddArgs {
    /// Name of the device you created, matching the reserved name
    pub name: String,
    #[command(flatten)]
    pub lease: LeaseArgs,
    #[command(flatten)]
    pub output: OutputArgs,
}

#[derive(Debug, Args)]
pub struct DeviceForgetArgs {
    /// Device UDID, AVD name or simulator name
    pub device: String,
    #[command(flatten)]
    pub output: OutputArgs,
}

#[derive(Debug, Args)]
pub struct RunArgs {
    #[command(flatten)]
    pub lease: LeaseArgs,
    #[command(flatten)]
    pub output: OutputArgs,
    /// Command to run while the lease is held
    #[arg(last = true, required = true)]
    pub command: Vec<String>,
}

#[derive(Debug, Args)]
pub struct HeavyArgs {
    /// Slots this job takes, for example 2 for two simulators at once
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..))]
    pub weight: u32,
    /// Name shown in `simporter status`; defaults to the program name
    #[arg(long)]
    pub label: Option<String>,
    /// Who holds the slot; defaults to `SIMPORTER_OWNER`, then to the parent process id
    #[arg(long)]
    pub owner: Option<String>,
    /// Give up after waiting this long for a slot, for example 45s, 30m or 2h; exits with code 6
    #[arg(long, value_name = "DURATION")]
    pub wait_timeout: Option<String>,
    #[command(flatten)]
    pub output: OutputArgs,
    /// Command to run while the slot is held
    #[arg(last = true, required = true)]
    pub command: Vec<String>,
}

#[derive(Debug, Args, Clone, Copy)]
pub struct OutputArgs {
    /// Emit machine readable JSON instead of text
    #[arg(long)]
    pub json: bool,
}

pub fn resolve_owner(explicit: Option<&str>) -> Result<String> {
    let from_env = std::env::var("SIMPORTER_OWNER").ok();
    pick_owner(explicit, from_env.as_deref())
}

fn pick_owner(explicit: Option<&str>, from_env: Option<&str>) -> Result<String> {
    [explicit, from_env]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            crate::exit::usage("no owner given; pass --owner <id> or set SIMPORTER_OWNER")
        })
}

pub fn parse_duration(raw: &str) -> Result<u64> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        bail!("empty duration");
    }
    let (digits, multiplier) = match trimmed.chars().last() {
        Some('s') => (&trimmed[..trimmed.len() - 1], 1),
        Some('m') => (&trimmed[..trimmed.len() - 1], 60),
        Some('h') => (&trimmed[..trimmed.len() - 1], 3600),
        _ => (trimmed, 1),
    };
    let value: u64 = digits
        .parse()
        .map_err(|_| anyhow::anyhow!("could not read {raw:?} as a duration like 45s, 30m or 2h"))?;
    if value == 0 {
        bail!("duration must be greater than zero");
    }
    Ok(value * multiplier)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_accept_suffixes() {
        assert_eq!(parse_duration("45s").unwrap(), 45);
        assert_eq!(parse_duration("30m").unwrap(), 1800);
        assert_eq!(parse_duration("2h").unwrap(), 7200);
        assert_eq!(parse_duration("900").unwrap(), 900);
    }

    #[test]
    fn bad_durations_are_rejected() {
        assert!(parse_duration("").is_err());
        assert!(parse_duration("0m").is_err());
        assert!(parse_duration("later").is_err());
    }

    #[test]
    fn explicit_owner_wins_over_the_environment() {
        assert_eq!(
            pick_owner(Some("agent-7"), Some("from-env")).unwrap(),
            "agent-7"
        );
    }

    #[test]
    fn the_environment_is_the_fallback() {
        assert_eq!(pick_owner(None, Some("from-env")).unwrap(), "from-env");
    }

    #[test]
    fn owners_are_trimmed() {
        assert_eq!(pick_owner(Some("  agent-7\n"), None).unwrap(), "agent-7");
    }

    #[test]
    fn a_blank_owner_falls_through_to_the_environment() {
        assert_eq!(
            pick_owner(Some("   "), Some("from-env")).unwrap(),
            "from-env"
        );
    }

    #[test]
    fn no_owner_anywhere_is_an_error() {
        assert!(pick_owner(Some("   "), None).is_err());
        assert!(pick_owner(None, Some("")).is_err());
        assert!(pick_owner(None, None).is_err());
    }

    #[test]
    fn cli_parses_the_run_wrapper() {
        let cli = Cli::try_parse_from([
            "simporter",
            "run",
            "--ports",
            "metro",
            "--",
            "npx",
            "expo",
            "start",
        ])
        .unwrap();
        let Command::Run(args) = cli.command else {
            panic!("expected run")
        };
        assert_eq!(args.command, vec!["npx", "expo", "start"]);
        assert_eq!(args.lease.ports, vec!["metro"]);
    }

    #[test]
    fn claim_defaults_to_ios() {
        let cli = Cli::try_parse_from(["simporter", "claim"]).unwrap();
        let Command::Claim(args) = cli.command else {
            panic!("expected claim")
        };
        assert_eq!(args.lease.platform, Platform::Ios);
    }
}
