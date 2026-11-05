# simporter

`simporter` hands out exclusive, self-expiring leases on simulators, emulators and TCP ports so that
several AI agents can work on the same machine without colliding.

It solves two failures:

1. Two agents drive the same simulator and their actions interleave.
2. An agent finds something listening on `:8081`, assumes it is its own Metro, and reads another
   agent's bundle and logs.
3. Too many agents build or boot simulators at the same time, and the machine stalls.

`simporter` does **not** boot devices, start Metro or install apps. It only answers "what may I use".

## The pool is name-scoped

A device is claimable only if its name carries the `Agent` suffix:

- iOS simulator: `iPhone 17 Pro Agent 1`
- Android AVD: `Pixel_9_Agent_1`

Everything else is reported as `unmanaged` and can never be leased. Your own simulators are
invisible to every agent.

## Agent protocol

```sh
export SIMPORTER_OWNER="$SESSION_ID"

simporter run --ports metro,inspector -- npx react-native start --port "$SIMPORTER_PORT_METRO"
```

`run` takes the lease, exports it into the child environment, renews it while the child lives, and
releases it when the child exits. Use it whenever one command spans the whole session.

For a lease that outlives a single command:

```sh
simporter claim --ports metro --json     # exit 0, prints the lease
simporter renew                          # push the expiry out
simporter release --owner "$SIMPORTER_OWNER" --all
```

Exported into the child, and returned under `env` in the JSON payload:

| Variable | Meaning |
| --- | --- |
| `SIMPORTER_CLAIM_ID` | lease id, for `renew` and `release` |
| `SIMPORTER_DEVICE` | the id argent wants: UDID on iOS, adb serial on Android when booted |
| `SIMPORTER_DEVICE_ID` | stable id: UDID on iOS, AVD name on Android |
| `SIMPORTER_DEVICE_NAME` | human readable device name |
| `SIMPORTER_DEVICE_PLATFORM` | `ios` or `android` |
| `SIMPORTER_PORT_<NAME>` | one per requested port, for example `SIMPORTER_PORT_METRO` |

## Holding more than one device

One claim covers one device plus any number of ports. Claim again for a second device; each call
returns an independent lease with its own claim id, and one owner may hold as many as it needs.

```sh
simporter claim --ports metro          --label sender
simporter claim --ports metro_receiver --label receiver
```

Give each claim distinct port names. Names become environment variables, so two leases that both
call a port `metro` both export `SIMPORTER_PORT_METRO` and the second value wins. simporter prints
a warning on stderr when a new claim reuses a port name you already hold.

`renew` and `release --owner <id> --all` act on every lease that owner holds. `release <claim-id>`
gives back one. `run` manages exactly one lease, its own.

## When the pool is empty

`simporter` never creates a device for you. It reserves a unique name and tells you the exact command to
run, then you register what you created:

```console
$ simporter claim --ports metro
no free ios device in the agent pool
reserved name  iPhone 17 Pro Agent 1
create it      xcrun simctl create 'iPhone 17 Pro Agent 1' com.apple.CoreSimulator.SimDeviceType.iPhone-17-Pro com.apple.CoreSimulator.SimRuntime.iOS-26-5
then claim it  simporter device add 'iPhone 17 Pro Agent 1'
$ echo $?
3
```

The reservation is written under the state lock, so two agents that hit an empty pool at the same
moment are handed `Agent 1` and `Agent 2`, never the same name. Reservations expire after 10
minutes. `simporter device add` verifies the device exists, checks the name is in the pool, consumes the
reservation and claims the device in one locked step.

The suggested device type and runtime are the configured default plus the newest installed runtime.
Override with `--device-type` and `--runtime`. On Android, `--device-type` sets the AVD name base and
the hardware profile comes from `default_avd_profile` in the config.

## Heavy work queue

Builds and simulator boots use a lot of CPU. When several agents start them at the same time, the
machine stalls. `simporter heavy` puts such commands in one queue with a fixed number of slots:

```sh
simporter heavy -- xcodebuild -workspace App.xcworkspace -scheme App build
simporter heavy --weight 2 --label two-sims -- sh -c 'xcrun simctl boot A && xcrun simctl boot B'
```

- The command waits until enough slots are free, runs, and gives the slots back when it exits. If
  `simporter` itself dies, the next command finds the process gone and frees the slots.
- The queue is strict first in, first out. A job never starts before a job that queued earlier,
  also when its weight would fit. A job with a large weight therefore never waits without end.
- `heavy_slots` in the config sets the number of slots (default 2). `--weight` must not exceed it.
- `--wait-timeout 10m` stops waiting after that time: the job leaves the queue, the command does
  not run, and `simporter` exits with `6` and names the jobs that hold the slots. Without it, a job
  waits until it gets its slots.
- The child gets `SIMPORTER_HEAVY_JOB`. A `simporter heavy` inside a heavy job runs at once and
  takes no slot, so a wrapped script that wraps its own steps cannot wait for itself.
- `--owner` defaults to `SIMPORTER_OWNER`, then to the parent process id. `--label` defaults to the
  program name. Both show in `simporter status`, with the time each job has run or waited.
- `heavy` takes no device and no port. To boot a leased simulator, nest the two:
  `simporter run -- sh -c 'simporter heavy -- xcrun simctl boot "$SIMPORTER_DEVICE"'`.

The queue limits how many heavy jobs run at once. It does not make one job cheaper.

### Hook for Claude Code

`simporter hook` is a `PreToolUse` hook. It reads the hook input on stdin. When a Bash command runs
`xcodebuild`, Gradle, `cargo build`/`test`/`bench`/`install`, `swift build -c release`,
`pod install`, `xcrun simctl boot`, `react-native run-ios`/`run-android` or
`expo run:ios`/`run:android` outside `simporter heavy`, it denies the command and tells the agent
to rerun it through the queue. Add it to `~/.claude/settings.json` by hand:

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Bash",
        "hooks": [{ "type": "command", "command": "simporter hook", "timeout": 10 }]
      }
    ]
  }
}
```

The hook reads only the command line. It does not see heavy work inside scripts, for example
`yarn ios` or `bash -c "..."`.

## Commands

```
simporter claim   [--platform ios|android] [--device <id>] [--no-device]
                  [--ports a,b] [--n-ports N] [--ttl 30m] [--owner <id>]
                  [--owner-pid <pid>] [--label <text>] [--json]
simporter release <claim-id> | --owner <id> --all
simporter renew   [<claim-id>] [--ttl 30m]
simporter status  [--platform ios|android] [--json]
simporter device  list | add <name> | forget <device>
simporter run     [claim flags] -- <cmd> ...
simporter heavy   [--weight N] [--label <text>] [--owner <id>] [--wait-timeout 10m]
                  [--json] -- <cmd> ...
simporter hook
simporter gc
simporter doctor
```

Exit codes: `0` granted, `1` refused or failed, `2` usage, `3` pool exhausted (create a device),
`4` no free port in range, `5` timed out waiting for the state lock, `6` timed out waiting for a
heavy slot.

## How exclusivity is enforced

- Every mutation takes an exclusive `flock` on `~/.simporter/state.lock`, does a full
  read-modify-write of `state.json`, and writes it back through a temp file and `rename`. The kernel
  releases `flock` if a process dies, so the lock cannot be orphaned.
- A port is granted only after the registry says it is unclaimed **and** a live bind proves it is
  free. The probe binds `127.0.0.1`, `0.0.0.0` and `[::]`, because on macOS a bind to `127.0.0.1`
  succeeds even when another process holds `0.0.0.0` on that port. Checking loopback alone would
  hand out a busy port.
- A lease ends when its TTL expires or, when `owner_pid` is set, when that process is gone.
  `gc` runs at the start of every command.

## Configuration

`~/.simporter/config.json`, created on first run:

```json
{
  "agent_suffix": "Agent",
  "default_device_type": "iPhone 17 Pro",
  "default_avd_base": "Pixel_9",
  "default_avd_profile": null,
  "port_range_start": 8081,
  "port_range_end": 8199,
  "default_ttl_secs": 1800,
  "reservation_ttl_secs": 600,
  "lock_timeout_secs": 10,
  "heavy_slots": 2
}
```

`SIMPORTER_HOME` moves the state directory. `SIMPORTER_OWNER` supplies the default owner.
`SIMPORTER_DEVICE_SOURCE` points at a JSON file of devices and replaces real discovery; it exists for
tests and dry runs.

## Telling agents to use it

`simporter rule` prints a short rule that tells any agent to claim before it touches a device or a
port. The rule stays loaded at all times. The full guide is a skill that agents load when they
need it; `simporter rule --skill` prints it.

`simporter rule --install` writes both into every agent harness it detects. The rule goes into a
dedicated file where the harness reads a rules directory, and into a delimited block inside a shared
instructions file otherwise, so nothing already in that file is lost. Re-running replaces the block
in place. For Claude Code the skill goes into `~/.claude/skills/simporter/SKILL.md`. Other harnesses
get only the rule; it tells the agent to run `simporter rule --skill` when the skill is missing.
`--dry-run` lists every rule and skill change without writing.

If you keep your agent configuration in a dotfiles repo, point simporter at the canonical files and
let your own linker place them:

```sh
simporter rule --install --target ~/dotfiles/AGENTS.md --skill-target ~/dotfiles/skills
```

`--target` and `--skill-target` replace harness detection rather than adding to it. Writes follow
symlinks, so a managed file that is a link into a dotfiles repo keeps its link and the repo copy is
updated.

## Install

```sh
cargo install simporter                                     # from crates.io
cargo install --git https://github.com/tjzel/simporter      # from source
cargo install --path .                                      # from a checkout
```

macOS and Linux. The lease uses `flock` and `kill(pid, 0)`, so the crate does not build on Windows.
Device discovery needs `xcrun` on macOS and the Android SDK on either; port leases work without both.

## Develop

```sh
cargo fmt
cargo lint      # clippy --all-targets -D warnings
cargo test
```
