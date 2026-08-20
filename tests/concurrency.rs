use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

const AGENTS: usize = 8;

#[test]
fn concurrent_port_claims_never_overlap() {
    let home = sandbox("ports");
    let outputs = fan_out(&home, None, |index| {
        vec![
            "claim".to_string(),
            "--no-device".to_string(),
            "--n-ports".to_string(),
            "2".to_string(),
            "--owner".to_string(),
            format!("agent-{index}"),
            "--json".to_string(),
        ]
    });

    let mut seen: HashSet<u16> = HashSet::new();
    let mut claimed = 0;
    for output in &outputs {
        assert_eq!(output.status.code(), Some(0), "{}", text(output));
        let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let ports = payload["ports"].as_object().unwrap();
        assert_eq!(ports.len(), 2);
        for port in ports.values() {
            let port = u16::try_from(port.as_u64().unwrap()).unwrap();
            assert!(seen.insert(port), "port {port} was handed out twice");
        }
        claimed += 1;
    }
    assert_eq!(claimed, AGENTS);
    assert_eq!(seen.len(), AGENTS * 2);
}

#[test]
fn a_two_device_pool_admits_exactly_two_agents() {
    let home = sandbox("devices");
    let pool = write_pool(
        &home,
        r#"[
          {"id": "UDID-1", "name": "iPhone 17 Pro Agent 1", "platform": "ios"},
          {"id": "UDID-2", "name": "iPhone 17 Pro Agent 2", "platform": "ios"},
          {"id": "UDID-3", "name": "iPhone 17 Pro", "platform": "ios"},
          {"id": "UDID-4", "name": "iPad (A16)", "platform": "ios"}
        ]"#,
    );
    let outputs = fan_out(&home, Some(&pool), |index| {
        vec![
            "claim".to_string(),
            "--platform".to_string(),
            "ios".to_string(),
            "--ports".to_string(),
            "metro".to_string(),
            "--owner".to_string(),
            format!("agent-{index}"),
            "--json".to_string(),
        ]
    });

    let mut devices: HashSet<String> = HashSet::new();
    let mut reserved: Vec<String> = Vec::new();
    for output in &outputs {
        let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        match output.status.code() {
            Some(0) => {
                let id = payload["device"]["id"].as_str().unwrap().to_string();
                assert!(devices.insert(id), "a device was leased twice");
            }
            Some(3) => {
                assert_eq!(payload["status"], "pool_exhausted");
                reserved.push(payload["reserved_name"].as_str().unwrap().to_string());
            }
            other => panic!("unexpected exit {other:?}: {}", text(output)),
        }
    }

    assert_eq!(devices.len(), 2, "only the two Agent devices are claimable");
    assert_eq!(reserved.len(), AGENTS - 2);
    let unique: HashSet<&String> = reserved.iter().collect();
    assert_eq!(
        unique.len(),
        reserved.len(),
        "two agents were told to create the same name"
    );
    for name in &reserved {
        assert!(
            name.starts_with("iPhone 17 Pro Agent "),
            "unexpected reserved name {name}"
        );
        assert_ne!(name, "iPhone 17 Pro Agent 1");
        assert_ne!(name, "iPhone 17 Pro Agent 2");
    }
}

#[test]
fn a_device_the_human_owns_is_never_claimable() {
    let home = sandbox("unmanaged");
    let pool = write_pool(
        &home,
        r#"[{"id": "UDID-9", "name": "iPhone 17 Pro", "platform": "ios"}]"#,
    );
    let refused = simporter(
        &home,
        Some(&pool),
        &[
            "claim",
            "--device",
            "iPhone 17 Pro",
            "--owner",
            "solo",
            "--json",
        ],
    );
    assert_eq!(refused.status.code(), Some(1), "{}", text(&refused));
    assert!(
        text(&refused).contains("not part of the agent pool"),
        "{}",
        text(&refused)
    );

    let exhausted = simporter(&home, Some(&pool), &["claim", "--owner", "solo", "--json"]);
    assert_eq!(exhausted.status.code(), Some(3), "{}", text(&exhausted));
}

#[test]
fn a_lease_whose_owner_process_died_is_reclaimed() {
    let home = sandbox("dead-owner");
    let pool = write_pool(
        &home,
        r#"[{"id": "UDID-1", "name": "iPhone 17 Pro Agent 1", "platform": "ios"}]"#,
    );

    let mut victim = command(&home, Some(&pool))
        .args([
            "run", "--ports", "metro", "--owner", "doomed", "--", "sleep", "60",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_for(
        || holds_a_lease(&home, Some(&pool)),
        "the run wrapper to take its lease",
    );

    kill_hard(victim.id());
    let _ = victim.wait();

    let after = simporter(
        &home,
        Some(&pool),
        &["claim", "--ports", "metro", "--owner", "next", "--json"],
    );
    assert_eq!(after.status.code(), Some(0), "{}", text(&after));
    let payload: serde_json::Value = serde_json::from_slice(&after.stdout).unwrap();
    assert_eq!(payload["device"]["id"], "UDID-1");
}

#[test]
fn the_run_wrapper_exports_its_lease_and_releases_on_exit() {
    let home = sandbox("run-wrapper");
    let pool = write_pool(
        &home,
        r#"[{"id": "UDID-1", "name": "iPhone 17 Pro Agent 1", "platform": "ios"}]"#,
    );
    let output = simporter(
        &home,
        Some(&pool),
        &[
            "run",
            "--ports",
            "metro",
            "--owner",
            "runner",
            "--",
            "sh",
            "-c",
            "echo $SIMPORTER_DEVICE_NAME/$SIMPORTER_PORT_METRO",
        ],
    );
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    let printed = String::from_utf8_lossy(&output.stdout);
    assert!(printed.contains("iPhone 17 Pro Agent 1/8081"), "{printed}");

    let status = simporter(&home, Some(&pool), &["status", "--json"]);
    let payload: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(
        payload["claims"].as_array().unwrap().len(),
        0,
        "the lease outlived the child"
    );
}

#[test]
fn a_released_lease_frees_its_device_and_ports() {
    let home = sandbox("release");
    let pool = write_pool(
        &home,
        r#"[{"id": "UDID-1", "name": "iPhone 17 Pro Agent 1", "platform": "ios"}]"#,
    );
    let first = simporter(
        &home,
        Some(&pool),
        &["claim", "--ports", "metro", "--owner", "a", "--json"],
    );
    assert_eq!(first.status.code(), Some(0), "{}", text(&first));

    let blocked = simporter(
        &home,
        Some(&pool),
        &["claim", "--ports", "metro", "--owner", "b", "--json"],
    );
    assert_eq!(blocked.status.code(), Some(3), "{}", text(&blocked));

    let released = simporter(&home, Some(&pool), &["release", "--owner", "a", "--all"]);
    assert_eq!(released.status.code(), Some(0), "{}", text(&released));

    let second = simporter(
        &home,
        Some(&pool),
        &["claim", "--ports", "metro", "--owner", "b", "--json"],
    );
    assert_eq!(second.status.code(), Some(0), "{}", text(&second));
    let payload: serde_json::Value = serde_json::from_slice(&second.stdout).unwrap();
    assert_eq!(payload["ports"]["metro"], 8081);
}

#[test]
fn a_missing_owner_is_a_usage_error() {
    let home = sandbox("no-owner");
    let output = simporter(&home, None, &["claim", "--no-device", "--n-ports", "1"]);
    assert_eq!(output.status.code(), Some(2), "{}", text(&output));
}

fn fan_out(home: &Path, pool: Option<&Path>, build: impl Fn(usize) -> Vec<String>) -> Vec<Output> {
    let children: Vec<_> = (0..AGENTS)
        .map(|index| {
            command(home, pool)
                .args(build(index))
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    children
        .into_iter()
        .map(|child| child.wait_with_output().unwrap())
        .collect()
}

fn simporter(home: &Path, pool: Option<&Path>, args: &[&str]) -> Output {
    command(home, pool).args(args).output().unwrap()
}

fn command(home: &Path, pool: Option<&Path>) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_simporter"));
    command.env("SIMPORTER_HOME", home);
    command.env_remove("SIMPORTER_OWNER");
    match pool {
        Some(path) => command.env("SIMPORTER_DEVICE_SOURCE", path),
        None => command.env_remove("SIMPORTER_DEVICE_SOURCE"),
    };
    command
}

fn holds_a_lease(home: &Path, pool: Option<&Path>) -> bool {
    let output = simporter(home, pool, &["status", "--json"]);
    serde_json::from_slice::<serde_json::Value>(&output.stdout)
        .ok()
        .and_then(|payload| Some(!payload["claims"].as_array()?.is_empty()))
        .unwrap_or(false)
}

fn wait_for(mut condition: impl FnMut() -> bool, what: &str) {
    for _ in 0..100 {
        if condition() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!("timed out waiting for {what}");
}

fn kill_hard(pid: u32) {
    let status = Command::new("kill")
        .args(["-KILL", &pid.to_string()])
        .status()
        .expect("failed to run kill");
    assert!(status.success(), "could not kill {pid}");
}

fn write_pool(home: &Path, body: &str) -> PathBuf {
    let path = home.join("pool.json");
    std::fs::write(&path, body).unwrap();
    path
}

fn sandbox(label: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(".tmp/it")
        .join(format!("{label}-{}-{seq}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn text(output: &Output) -> String {
    format!(
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}
