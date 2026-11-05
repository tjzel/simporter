use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_simporter");

#[test]
fn one_slot_never_runs_two_jobs_at_once() {
    let home = sandbox("one-slot", 1);
    let guard = home.join("inside");
    let script = format!(
        "mkdir '{0}' || exit 99; sleep 0.3; rmdir '{0}'",
        guard.display()
    );
    let children: Vec<Child> = (0..4)
        .map(|index| {
            heavy(&home)
                .args([
                    "--owner",
                    &format!("agent-{index}"),
                    "--",
                    "sh",
                    "-c",
                    &script,
                ])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    for child in children {
        let output = child.wait_with_output().unwrap();
        assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    }
    assert_eq!(heavy_jobs(&home).len(), 0, "a slot outlived its job");
}

#[test]
fn the_child_exit_code_passes_through() {
    let home = sandbox("exit-code", 2);
    let output = heavy(&home)
        .args(["--", "sh", "-c", "exit 7"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(7), "{}", text(&output));
}

#[test]
fn a_slot_whose_holder_died_is_given_to_the_next_job() {
    let home = sandbox("dead-holder", 1);
    let mut holder = heavy(&home)
        .args(["--owner", "doomed", "--", "sleep", "10"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_for(
        || heavy_jobs(&home).len() == 1,
        "the holder to take its slot",
    );
    Command::new("kill")
        .args(["-KILL", &holder.id().to_string()])
        .status()
        .unwrap();
    let _ = holder.wait();

    let started = Instant::now();
    let next = heavy(&home).args(["--", "true"]).output().unwrap();
    assert_eq!(next.status.code(), Some(0), "{}", text(&next));
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn a_heavy_command_inside_a_heavy_job_does_not_wait_for_itself() {
    let home = sandbox("nested", 1);
    let output = heavy(&home)
        .args([
            "--",
            BIN,
            "heavy",
            "--",
            "sh",
            "-c",
            "echo inner:$SIMPORTER_HEAVY_JOB",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    assert!(text(&output).contains("inner:h"), "{}", text(&output));
    assert!(text(&output).contains("already inside heavy job"));
}

#[test]
fn status_lists_running_and_waiting_jobs() {
    let home = sandbox("status", 1);
    let mut first = heavy(&home)
        .args([
            "--owner", "first", "--label", "build-a", "--", "sleep", "10",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_for(|| heavy_jobs(&home).len() == 1, "the first job to start");
    let second = heavy(&home)
        .args([
            "--owner", "second", "--label", "build-b", "--json", "--", "true",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_for(|| heavy_jobs(&home).len() == 2, "the second job to queue");

    let jobs = heavy_jobs(&home);
    assert_eq!(jobs[0]["owner"], "first");
    assert_eq!(jobs[0]["label"], "build-a");
    assert_eq!(jobs[0]["state"], "running");
    assert_eq!(jobs[1]["owner"], "second");
    assert_eq!(jobs[1]["state"], "waiting");
    let text_status = simporter(&home, &["status"]);
    let printed = String::from_utf8_lossy(&text_status.stdout);
    assert!(printed.contains("heavy slots 1/1 in use"), "{printed}");
    assert!(printed.contains("waiting"), "{printed}");

    let _ = first.kill();
    let _ = first.wait();
    let output = second.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(0));
    let events = String::from_utf8_lossy(&output.stderr);
    assert!(events.contains(r#""event":"waiting""#), "{events}");
    assert!(events.contains(r#""event":"started""#), "{events}");
}

#[test]
fn a_job_that_waits_too_long_gives_up_and_leaves_the_queue() {
    let home = sandbox("wait-timeout", 1);
    let mut holder = heavy(&home)
        .args([
            "--owner",
            "holder",
            "--label",
            "long-build",
            "--",
            "sleep",
            "10",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_for(
        || heavy_jobs(&home).len() == 1,
        "the holder to take its slot",
    );

    let started = Instant::now();
    let output = heavy(&home)
        .args(["--owner", "waiter", "--wait-timeout", "1s", "--", "true"])
        .output()
        .unwrap();
    let elapsed = started.elapsed();
    let jobs = heavy_jobs(&home);
    let _ = holder.kill();
    let _ = holder.wait();

    assert_eq!(output.status.code(), Some(6), "{}", text(&output));
    assert!(
        elapsed >= Duration::from_secs(1),
        "gave up after {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "gave up after {elapsed:?}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("gave up after waiting"), "{stderr}");
    assert!(stderr.contains("owner=holder long-build"), "{stderr}");
    assert_eq!(jobs.len(), 1, "the waiter stayed in the queue: {jobs:?}");
    assert_eq!(jobs[0]["owner"], "holder");
}

#[test]
fn a_free_slot_starts_at_once_despite_a_wait_timeout() {
    let home = sandbox("timeout-free", 1);
    let output = heavy(&home)
        .args(["--wait-timeout", "1s", "--", "sh", "-c", "exit 3"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3), "{}", text(&output));
}

#[test]
fn a_bad_wait_timeout_is_a_usage_error() {
    let home = sandbox("timeout-bad", 1);
    let output = heavy(&home)
        .args(["--wait-timeout", "soon", "--", "true"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "{}", text(&output));
    assert_eq!(heavy_jobs(&home).len(), 0);
}

#[test]
fn a_weight_above_the_slot_count_is_a_usage_error() {
    let home = sandbox("too-heavy", 2);
    let output = heavy(&home)
        .args(["--weight", "3", "--", "true"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "{}", text(&output));
    assert_eq!(heavy_jobs(&home).len(), 0);
}

#[test]
fn the_hook_denies_an_unwrapped_build_and_allows_a_wrapped_one() {
    let denied =
        hook(r#"{"tool_name":"Bash","tool_input":{"command":"cd ios && xcodebuild build"}}"#);
    let payload: serde_json::Value = serde_json::from_slice(&denied.stdout).unwrap();
    let output = &payload["hookSpecificOutput"];
    assert_eq!(output["hookEventName"], "PreToolUse");
    assert_eq!(output["permissionDecision"], "deny");
    assert!(
        output["permissionDecisionReason"]
            .as_str()
            .unwrap()
            .contains("simporter heavy -- xcodebuild build")
    );

    for allowed in [
        r#"{"tool_name":"Bash","tool_input":{"command":"simporter heavy -- xcodebuild build"}}"#,
        r#"{"tool_name":"Read","tool_input":{"file_path":"/x"}}"#,
        "not json",
    ] {
        let output = hook(allowed);
        assert_eq!(output.status.code(), Some(0));
        assert!(output.stdout.is_empty(), "{allowed} was denied");
    }
}

fn heavy(home: &Path) -> Command {
    let mut command = command(home);
    command.arg("heavy");
    command
}

fn hook(input: &str) -> Output {
    let mut child = Command::new(BIN)
        .arg("hook")
        .env("SIMPORTER_HOME", "/nonexistent/simporter-hook-home")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn heavy_jobs(home: &Path) -> Vec<serde_json::Value> {
    let output = simporter(home, &["status", "--json"]);
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    payload["heavy"]["jobs"].as_array().unwrap().clone()
}

fn simporter(home: &Path, args: &[&str]) -> Output {
    command(home).args(args).output().unwrap()
}

fn command(home: &Path) -> Command {
    let mut command = Command::new(BIN);
    command.env("SIMPORTER_HOME", home);
    command.env("SIMPORTER_DEVICE_SOURCE", home.join("pool.json"));
    command.env_remove("SIMPORTER_OWNER");
    command.env_remove("SIMPORTER_HEAVY_JOB");
    command
}

fn wait_for(mut condition: impl FnMut() -> bool, what: &str) {
    for _ in 0..100 {
        if condition() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("timed out waiting for {what}");
}

fn sandbox(label: &str, slots: u32) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(".tmp/it-heavy")
        .join(format!("{label}-{}-{seq}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    std::fs::write(
        path.join("config.json"),
        format!(r#"{{"heavy_slots": {slots}}}"#),
    )
    .unwrap();
    std::fs::write(path.join("pool.json"), "[]").unwrap();
    path
}

fn text(output: &Output) -> String {
    format!(
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}
