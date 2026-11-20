use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_simporter");

#[test]
fn install_with_dry_run_writes_nothing() {
    let root = sandbox("install-dry-run");
    let rule = root.join("AGENTS.md");
    let skills = root.join("skills");
    let output = rule_command(&rule, &skills, &["--install", "--dry-run"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("create"), "{stdout}");
    assert!(stdout.contains("nothing was written"), "{stdout}");
    assert!(!rule.exists(), "the dry run wrote the rule");
    assert!(!skills.exists(), "the dry run wrote the skill");
}

#[test]
fn dry_run_alone_writes_nothing() {
    let root = sandbox("dry-run");
    let rule = root.join("AGENTS.md");
    let skills = root.join("skills");
    let output = rule_command(&rule, &skills, &["--dry-run"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    assert!(!rule.exists(), "the dry run wrote the rule");
    assert!(!skills.exists(), "the dry run wrote the skill");
}

#[test]
fn install_writes_the_rule_and_the_skill() {
    let root = sandbox("install");
    let rule = root.join("AGENTS.md");
    let skills = root.join("skills");
    let output = rule_command(&rule, &skills, &["--install"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.contains("nothing was written"), "{stdout}");
    assert!(rule.exists(), "the install did not write the rule");
    assert!(
        skills.join("simporter/SKILL.md").exists(),
        "the install did not write the skill"
    );
}

fn rule_command(rule: &Path, skills: &Path, flags: &[&str]) -> Output {
    Command::new(BIN)
        .arg("rule")
        .args(flags)
        .arg("--target")
        .arg(rule)
        .arg("--skill-target")
        .arg(skills)
        .output()
        .unwrap()
}

fn sandbox(label: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(".tmp/it-rule")
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
