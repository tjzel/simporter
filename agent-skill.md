---
name: simporter
description: How to claim a simulator, emulator or TCP port through simporter so parallel agents never collide. Use before you boot, install to or drive an iOS simulator or Android emulator, start Metro, a dev server, a debugger or anything else that listens on a TCP port, or connect to a port you did not start yourself.
---

# simporter — claim simulators and ports before you use them

Several AI agents share this machine. They collide on simulators, emulators and TCP ports.
`simporter` is the broker that prevents it. Go through it. Do not skip it.

## When this applies

Hold a simporter lease before you do any of these:

- boot, install to, or drive an iOS simulator or Android emulator
- start Metro, a dev server, a debugger, or anything else that listens on a TCP port
- connect to a port you did not start yourself

## Rules

1. **Never use a port you were not granted.** Something already listening on 8081 is another
   agent's Metro, not yours. Do not attach to it. Do not assume it serves your project.
2. **Never free a port by killing its holder.** No `kill`, no `fuckport`, no `pkill -f metro`.
   The holder is another agent doing real work. Ask simporter for a different port.
3. **Never touch a device simporter did not give you.** Only devices whose name carries the
   `Agent` suffix are shared. Every other simulator belongs to the human at the keyboard.
4. **Release when you finish**, or hold the lease under `simporter run` so it releases itself.
5. **Run heavy work through `simporter heavy`.** See "Heavy work" below.

## How

Set an owner once per session. Any stable string works; your session id is ideal.

```sh
export SIMPORTER_OWNER="<your session id>"
```

Prefer wrapping the long-running process. The lease then lives exactly as long as the child and
is released even if you crash:

```sh
simporter run --ports metro -- npx react-native start --port "$SIMPORTER_PORT_METRO"
```

For a lease that spans many commands:

```sh
simporter claim --ports metro,inspector --json
# ... work ...
simporter release --owner "$SIMPORTER_OWNER" --all
```

Use what the claim gives you, never a default:

| variable | use |
| --- | --- |
| `SIMPORTER_DEVICE` | device id for the automation tool: UDID on iOS, adb serial on Android |
| `SIMPORTER_DEVICE_NAME` | human readable name, for reporting to the user |
| `SIMPORTER_PORT_<NAME>` | one per port you asked for, e.g. `SIMPORTER_PORT_METRO` |
| `SIMPORTER_CLAIM_ID` | for `simporter renew` and `simporter release` |

Pass the granted port everywhere: to `--port` on the bundler, and to every debugger or inspector
call that would otherwise default to 8081.

A lease lasts 30 minutes. For longer work run `simporter renew`.

## Holding more than one device

One claim covers one device and as many ports as you ask for. To work with two simulators at once,
claim twice. You get two independent leases, each with its own claim id:

```sh
simporter claim --ports metro          --label sender   --json
simporter claim --ports metro_receiver --label receiver --json
```

Three things matter when you hold several leases:

1. **Give every port a distinct name.** Port names become environment variables, so two claims that
   both name a port `metro` both export `SIMPORTER_PORT_METRO` and the second one silently wins.
   simporter warns you on stderr when this happens. Use `metro_sender` / `metro_receiver`, or read
   each claim's ports out of its own JSON instead of the environment.
2. **Keep the claim ids.** Match each device to its ports through its claim id, not through the
   environment. `--label` gives each lease a name you will recognise in `simporter status`.
3. **`simporter run` covers one device.** Its lease releases when the child exits; any extra leases
   you took by hand are yours to release.

`simporter renew` with no arguments renews every lease you hold. `simporter release --owner
"$SIMPORTER_OWNER" --all` releases all of them, including any pending device reservation. To give
back just one, `simporter release <claim-id>`.

Ask only for what you need. A device you hold is a device no other agent can use.

## Heavy work

Builds and simulator boots use a lot of CPU. Too many at once stall the machine for every agent.
Run each one through the heavy-work queue:

```sh
simporter heavy -- xcodebuild -workspace App.xcworkspace -scheme App build
simporter heavy -- ./gradlew assembleDebug
simporter heavy -- cargo build --release
simporter heavy --weight 2 --label two-sims -- sh -c 'xcrun simctl boot A && xcrun simctl boot B'
```

- Heavy work: Xcode, Gradle, Cargo and SwiftPM builds, `pod install`, `react-native run-ios` /
  `run-android`, `expo run:ios` / `run:android`, and simulator or emulator boots.
- The command waits in a first in, first out queue until a slot is free. While it waits, it prints
  who holds the slots. Do not kill other agents' jobs to get a slot.
- One job takes one slot. Use `--weight 2` for two simulators at once.
- `heavy` takes no device and no port. To boot a leased simulator, nest the two:
  `simporter run -- sh -c 'simporter heavy -- xcrun simctl boot "$SIMPORTER_DEVICE"'`.
- A `simporter heavy` inside a heavy job runs at once, so wrapping a script that wraps its own
  steps is safe.
- `simporter status` shows the running and waiting heavy jobs.

## Exit codes

| code | meaning | what to do |
| --- | --- | --- |
| 0 | granted | proceed, and use only what you were given |
| 1 | refused | read the message; the device is held, or is not in the shared pool |
| 2 | usage | you forgot `--owner` or `SIMPORTER_OWNER` |
| 3 | pool empty | run the printed create command, then `simporter device add '<reserved name>'` |
| 4 | no free port | stop and tell the user; do not take a port anyway |
| 5 | lock busy | another simporter command is mid-write; retry once |

On exit 3 simporter reserves a device name for you and prints the exact command that creates it.
That name is yours alone, so two agents never create the same device. Create it, register it with
`simporter device add`, and it stays in the pool for everyone later.

## Checking

- `simporter status` — every lease, every device, every heavy job, and who holds what
- `simporter doctor` — confirms simctl, adb, emulator and avdmanager are reachable
