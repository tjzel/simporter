use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

const BIN: &str = env!("CARGO_BIN_EXE_simporter");
const DEVICES: &str = r#"[
  {"id": "AGENT-1", "name": "iPhone 17 Pro Agent 1", "platform": "ios", "running": true},
  {"id": "AGENT-2", "name": "iPhone 17 Pro Agent 2", "platform": "ios", "running": false},
  {"id": "HUMAN-1", "name": "iPhone 17 Pro", "platform": "ios", "running": true}
]"#;

#[test]
fn a_never_leased_agent_device_is_shut_down_after_the_wait() {
    let home = sandbox("never-leased", 2);

    run(&home, &["gc"]);
    assert_eq!(shutdowns(&home), Vec::<String>::new());
    let status = run(&home, &["status"]);
    assert!(
        text(&status).contains("iPhone 17 Pro Agent 1 (no lease: shut down at"),
        "{}",
        text(&status)
    );

    std::thread::sleep(Duration::from_millis(2300));
    let gc = run(&home, &["gc"]);
    assert_eq!(
        shutdowns(&home),
        vec!["simctl shutdown AGENT-1".to_string()]
    );
    assert!(
        text(&gc).contains("shut down iPhone 17 Pro Agent 1"),
        "{}",
        text(&gc)
    );

    let log = std::fs::read_to_string(home.join("reaper.log")).unwrap();
    assert!(
        log.contains(
            "shut down iPhone 17 Pro Agent 1 (ios, AGENT-1): running without a lease since"
        ),
        "{log}"
    );
    let status = run(&home, &["status", "--json"]);
    let payload: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(payload["reaped"][0]["id"], "AGENT-1");
}

#[test]
fn a_leased_device_is_kept_and_its_wait_starts_at_the_release() {
    let home = sandbox("leased", 2);
    let claim = run(
        &home,
        &["claim", "--device", "AGENT-1", "--owner", "agent", "--json"],
    );
    assert_eq!(claim.status.code(), Some(0), "{}", text(&claim));

    std::thread::sleep(Duration::from_millis(2300));
    run(&home, &["gc"]);
    assert_eq!(shutdowns(&home), Vec::<String>::new());

    run(&home, &["release", "--owner", "agent", "--all"]);
    run(&home, &["gc"]);
    assert_eq!(shutdowns(&home), Vec::<String>::new());

    std::thread::sleep(Duration::from_millis(2300));
    run(&home, &["gc"]);
    assert_eq!(
        shutdowns(&home),
        vec!["simctl shutdown AGENT-1".to_string()]
    );
}

#[test]
fn a_zero_wait_turns_the_reaper_off() {
    let home = sandbox("disabled", 0);
    run(&home, &["gc"]);
    run(&home, &["gc"]);
    assert_eq!(shutdowns(&home), Vec::<String>::new());
    let status = run(&home, &["status"]);
    assert!(!text(&status).contains("shut down at"), "{}", text(&status));
}

#[test]
fn the_launchd_dry_run_writes_nothing() {
    let home = sandbox("launchd", 2);
    let fake_home = home.join("user-home");
    std::fs::create_dir_all(&fake_home).unwrap();
    let output = command(&home)
        .env("HOME", &fake_home)
        .args(["reaper", "--install", "--dry-run"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    let printed = text(&output);
    assert!(printed.contains("<string>gc</string>"), "{printed}");
    assert!(printed.contains("<integer>300</integer>"), "{printed}");
    assert!(!fake_home.join("Library").exists());
}

fn run(home: &Path, args: &[&str]) -> Output {
    let output = command(home).args(args).output().unwrap();
    assert!(
        matches!(output.status.code(), Some(0 | 1)),
        "simporter {args:?}: {}",
        text(&output)
    );
    output
}

fn shutdowns(home: &Path) -> Vec<String> {
    std::fs::read_to_string(home.join("xcrun.log"))
        .unwrap_or_default()
        .lines()
        .map(String::from)
        .collect()
}

fn command(home: &Path) -> Command {
    let mut command = Command::new(BIN);
    let path = std::env::var("PATH").unwrap_or_default();
    command.env("SIMPORTER_HOME", home);
    command.env("SIMPORTER_DEVICE_SOURCE", home.join("devices.json"));
    command.env("PATH", format!("{}:{path}", home.join("bin").display()));
    command.env_remove("SIMPORTER_OWNER");
    command
}

fn sandbox(label: &str, wait_secs: u64) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(".tmp/it-reaper")
        .join(format!("{label}-{}-{seq}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(path.join("bin")).unwrap();
    std::fs::write(
        path.join("config.json"),
        format!(
            r#"{{"reap_unleased_after_secs": {wait_secs}, "port_range_start": {}, "port_range_end": {}}}"#,
            18601 + seq * 10,
            18610 + seq * 10
        ),
    )
    .unwrap();
    std::fs::write(path.join("devices.json"), DEVICES).unwrap();
    let script = path.join("bin/xcrun");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\necho \"$*\" >> '{}'\n",
            path.join("xcrun.log").display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn text(output: &Output) -> String {
    format!(
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}
