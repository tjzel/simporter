use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;

use crate::cli::{OutputArgs, RuleArgs};
use crate::exit;

pub const TEXT: &str = include_str!("../agent-rule.md");
pub const SKILL: &str = include_str!("../agent-skill.md");

const SKILL_FILE: &str = "simporter/SKILL.md";

const BEGIN: &str = "<!-- BEGIN simporter (managed by `simporter rule --install`) -->";
const END: &str = "<!-- END simporter -->";

const HARNESSES: &[Harness] = &[
    Harness {
        name: "claude code",
        home: ".claude",
        file: "rules/simporter.md",
        mode: Mode::Own,
        skills: Some(".claude/skills"),
    },
    Harness {
        name: "codex",
        home: ".codex",
        file: "AGENTS.md",
        mode: Mode::Block,
        skills: None,
    },
    Harness {
        name: "kimi code",
        home: ".kimi-code",
        file: "AGENTS.md",
        mode: Mode::Block,
        skills: None,
    },
    Harness {
        name: "gemini cli",
        home: ".gemini",
        file: "GEMINI.md",
        mode: Mode::Block,
        skills: None,
    },
    Harness {
        name: "cursor",
        home: ".cursor",
        file: "rules/simporter.md",
        mode: Mode::Own,
        skills: None,
    },
    Harness {
        name: "opencode",
        home: ".config/opencode",
        file: "AGENTS.md",
        mode: Mode::Block,
        skills: None,
    },
];

pub fn run(args: &RuleArgs) -> Result<i32> {
    if !args.install && !args.dry_run {
        print!("{}", if args.skill { SKILL } else { TEXT });
        return Ok(exit::OK);
    }
    let plan = if args.target.is_empty() && args.skill_target.is_empty() {
        let home = dirs::home_dir().context("could not determine the home directory")?;
        detect(&home)?
    } else {
        explicit(&args.target, &args.skill_target)?
    };
    for entry in &plan {
        if args.install && entry.action != Action::Skipped {
            apply(entry).with_context(|| format!("failed to write {}", entry.path.display()))?;
        }
    }
    report(args.output, &plan, args.install);
    Ok(exit::OK)
}

fn explicit(rules: &[PathBuf], skills: &[PathBuf]) -> Result<Vec<Entry>> {
    let mut plan = Vec::new();
    for path in rules {
        plan.push(entry_for(
            "custom",
            Kind::Rule,
            path.clone(),
            infer_mode(path),
        )?);
    }
    for root in skills {
        plan.push(entry_for(
            "custom",
            Kind::Skill,
            root.join(SKILL_FILE),
            Mode::Own,
        )?);
    }
    Ok(plan)
}

fn detect(home: &Path) -> Result<Vec<Entry>> {
    let mut plan = Vec::new();
    for harness in HARNESSES {
        let root = home.join(harness.home);
        let path = root.join(harness.file);
        if !root.is_dir() {
            plan.push(Entry {
                harness: harness.name,
                kind: Kind::Rule,
                path,
                mode: harness.mode,
                action: Action::Skipped,
                detail: "harness not installed",
            });
            continue;
        }
        plan.push(entry_for(harness.name, Kind::Rule, path, harness.mode)?);
        if let Some(dir) = harness.skills {
            plan.push(entry_for(
                harness.name,
                Kind::Skill,
                home.join(dir).join(SKILL_FILE),
                Mode::Own,
            )?);
        }
    }
    Ok(plan)
}

fn entry_for(harness: &'static str, kind: Kind, path: PathBuf, mode: Mode) -> Result<Entry> {
    let existing = if path.exists() {
        Some(
            std::fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?,
        )
    } else {
        None
    };
    let wanted = render(kind, mode);
    let action = match &existing {
        None => Action::Created,
        Some(current) if *current == wanted && mode == Mode::Own => Action::Unchanged,
        Some(current) if mode == Mode::Block && splice(current) == *current => Action::Unchanged,
        Some(_) => Action::Updated,
    };
    let detail = match (kind, mode) {
        (Kind::Skill, _) => "skill with the full guide",
        (Kind::Rule, Mode::Own) => "dedicated rule file",
        (Kind::Rule, Mode::Block) => "managed block inside a shared instructions file",
    };
    Ok(Entry {
        harness,
        kind,
        path,
        mode,
        action,
        detail,
    })
}

fn apply(entry: &Entry) -> Result<()> {
    if let Some(parent) = entry.path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let body = match entry.mode {
        Mode::Own => render(entry.kind, Mode::Own),
        Mode::Block => match std::fs::read_to_string(&entry.path) {
            Ok(current) => splice(&current),
            Err(_) => render(Kind::Rule, Mode::Block),
        },
    };
    crate::state::write_atomic(&entry.path, &body)
}

fn splice(current: &str) -> String {
    let block = render(Kind::Rule, Mode::Block);
    let Some(start) = current.find(BEGIN) else {
        let separator = if current.is_empty() || current.ends_with("\n\n") {
            ""
        } else if current.ends_with('\n') {
            "\n"
        } else {
            "\n\n"
        };
        return format!("{current}{separator}{block}");
    };
    let Some(end) = current[start..]
        .find(END)
        .map(|offset| start + offset + END.len())
    else {
        return format!("{current}\n{block}");
    };
    format!(
        "{}{}{}",
        &current[..start],
        block.trim_end(),
        &current[end..]
    )
}

fn render(kind: Kind, mode: Mode) -> String {
    match (kind, mode) {
        (Kind::Skill, _) => SKILL.to_string(),
        (Kind::Rule, Mode::Own) => TEXT.to_string(),
        (Kind::Rule, Mode::Block) => format!("{BEGIN}\n\n{}\n{END}\n", TEXT.trim_end()),
    }
}

fn infer_mode(path: &Path) -> Mode {
    let shared = [
        "AGENTS.md",
        "CLAUDE.md",
        "GEMINI.md",
        "CONVENTIONS.md",
        "global_rules.md",
    ];
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if shared
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(name))
    {
        return Mode::Block;
    }
    Mode::Own
}

fn report(output: OutputArgs, plan: &[Entry], installed: bool) {
    if output.json {
        let payload: Vec<_> = plan
            .iter()
            .map(|entry| EntryPayload {
                harness: entry.harness,
                kind: entry.kind.as_str(),
                path: entry.path.display().to_string(),
                mode: entry.mode.as_str(),
                action: entry.action.as_str(),
                detail: entry.detail,
            })
            .collect();
        match serde_json::to_string_pretty(&payload) {
            Ok(body) => println!("{body}"),
            Err(error) => eprintln!("failed to encode json: {error}"),
        }
        return;
    }
    for entry in plan {
        println!(
            "{:<10} {:<6} {:<12} {}",
            entry.action.as_str(),
            entry.kind.as_str(),
            entry.harness,
            entry.path.display()
        );
    }
    let rules = plan
        .iter()
        .any(|entry| entry.kind == Kind::Rule && entry.action != Action::Skipped);
    let skills = plan.iter().any(|entry| entry.kind == Kind::Skill);
    if rules && !skills {
        println!();
        println!(
            "no skill is managed; add --skill-target <DIR> so agents can load the `simporter` skill"
        );
    }
    if !installed {
        println!();
        println!("nothing was written; rerun with --install to apply");
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Own,
    Block,
}

impl Mode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Own => "file",
            Self::Block => "block",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Rule,
    Skill,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Rule => "rule",
            Self::Skill => "skill",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Created,
    Updated,
    Unchanged,
    Skipped,
}

impl Action {
    fn as_str(self) -> &'static str {
        match self {
            Self::Created => "create",
            Self::Updated => "update",
            Self::Unchanged => "current",
            Self::Skipped => "skip",
        }
    }
}

#[derive(Debug)]
struct Harness {
    name: &'static str,
    home: &'static str,
    file: &'static str,
    mode: Mode,
    skills: Option<&'static str>,
}

#[derive(Debug)]
struct Entry {
    harness: &'static str,
    kind: Kind,
    path: PathBuf,
    mode: Mode,
    action: Action,
    detail: &'static str,
}

#[derive(Debug, Serialize)]
struct EntryPayload {
    harness: &'static str,
    kind: &'static str,
    path: String,
    mode: &'static str,
    action: &'static str,
    detail: &'static str,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rule_names_no_single_agent() {
        for text in [TEXT, SKILL] {
            let lowered = text.to_lowercase();
            for vendor in ["claude", "codex", "kimi", "gemini", "cursor"] {
                assert!(!lowered.contains(vendor), "the rule mentions {vendor}");
            }
        }
    }

    #[test]
    fn the_rule_is_short_and_points_at_the_skill() {
        assert!(TEXT.lines().count() <= 20);
        assert!(TEXT.contains("load the `simporter` skill"));
        assert!(TEXT.contains("simporter rule --skill"));
        for removed in ["## Exit codes", "SIMPORTER_OWNER", "simporter claim"] {
            assert!(!TEXT.contains(removed), "the rule still contains {removed}");
        }
    }

    #[test]
    fn the_skill_carries_the_full_guide() {
        assert!(SKILL.starts_with("---\nname: simporter\ndescription: "));
        let description = SKILL.lines().nth(2).unwrap().to_lowercase();
        for trigger in ["simulator", "emulator", "metro", "dev server", "port"] {
            assert!(
                description.contains(trigger),
                "the description lacks {trigger}"
            );
        }
        for section in [
            "SIMPORTER_OWNER",
            "simporter run --ports",
            "simporter claim --ports",
            "| `SIMPORTER_DEVICE` |",
            "## Holding more than one device",
            "## Exit codes",
            "simporter status",
            "simporter doctor",
        ] {
            assert!(SKILL.contains(section), "the skill lacks {section}");
        }
    }

    #[test]
    fn detection_installs_the_skill_only_for_claude_code() {
        let home = crate::testing::temp_dir("rule-detect");
        for dir in [".claude", ".codex", ".kimi-code"] {
            std::fs::create_dir_all(home.join(dir)).unwrap();
        }
        let plan = detect(&home).unwrap();
        let skills: Vec<_> = plan
            .iter()
            .filter(|entry| entry.kind == Kind::Skill)
            .map(|entry| (entry.harness, entry.path.clone()))
            .collect();
        assert_eq!(
            skills,
            vec![(
                "claude code",
                home.join(".claude/skills/simporter/SKILL.md")
            )]
        );
        let rules = plan
            .iter()
            .filter(|entry| entry.kind == Kind::Rule && entry.action != Action::Skipped)
            .count();
        assert_eq!(rules, 3);
    }

    #[test]
    fn explicit_targets_manage_rules_and_skills_separately() {
        let plan = explicit(&[PathBuf::from("/x/AGENTS.md")], &[]).unwrap();
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].kind, Kind::Rule);

        let plan = explicit(&[], &[PathBuf::from("/x/skills")]).unwrap();
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].kind, Kind::Skill);
        assert_eq!(plan[0].path, PathBuf::from("/x/skills/simporter/SKILL.md"));
    }

    #[test]
    fn installing_over_the_long_block_leaves_only_the_short_rule() {
        let root = crate::testing::temp_dir("rule-upgrade");
        let rule = root.join("AGENTS.md");
        let long = format!(
            "# mine\n\n{BEGIN}\n\n# simporter\n\n## Exit codes\n\n| 0 | granted |\n{END}\n\n# after\n"
        );
        std::fs::write(&rule, long).unwrap();

        let entries = explicit(std::slice::from_ref(&rule), &[root.join("skills")]).unwrap();
        assert_eq!(entries[0].action, Action::Updated);
        assert_eq!(entries[1].action, Action::Created);
        for entry in &entries {
            apply(entry).unwrap();
        }

        let written = std::fs::read_to_string(&rule).unwrap();
        assert!(written.starts_with("# mine\n\n"));
        assert!(written.ends_with("\n\n# after\n"));
        assert!(written.contains(TEXT.trim_end()));
        assert!(!written.contains("## Exit codes"));
        assert_eq!(written.matches(BEGIN).count(), 1);
        assert_eq!(written.matches(END).count(), 1);
        let skill = std::fs::read_to_string(root.join("skills/simporter/SKILL.md")).unwrap();
        assert_eq!(skill, SKILL);

        let again = explicit(&[rule], &[root.join("skills")]).unwrap();
        assert!(again.iter().all(|entry| entry.action == Action::Unchanged));
    }

    #[test]
    fn a_block_is_appended_to_an_existing_file() {
        let spliced = splice("# my notes\n\nkeep this\n");
        assert!(spliced.starts_with("# my notes\n\nkeep this\n"));
        assert!(spliced.contains(BEGIN));
        assert!(spliced.contains(END));
    }

    #[test]
    fn splicing_twice_changes_nothing() {
        let once = splice("# my notes\n");
        let twice = splice(&once);
        assert_eq!(once, twice);
    }

    #[test]
    fn a_stale_block_is_replaced_in_place() {
        let stale = format!("top\n\n{BEGIN}\nOLD TEXT\n{END}\n\nbottom\n");
        let fresh = splice(&stale);
        assert!(!fresh.contains("OLD TEXT"));
        assert!(fresh.starts_with("top\n\n"));
        assert!(fresh.ends_with("\n\nbottom\n"));
        assert_eq!(fresh.matches(BEGIN).count(), 1);
    }

    #[test]
    fn an_empty_file_gets_the_block_without_leading_blanks() {
        let spliced = splice("");
        assert!(spliced.starts_with(BEGIN));
    }

    #[test]
    fn shared_instruction_files_are_managed_as_blocks() {
        assert_eq!(infer_mode(Path::new("/x/AGENTS.md")), Mode::Block);
        assert_eq!(infer_mode(Path::new("/x/CLAUDE.md")), Mode::Block);
        assert_eq!(infer_mode(Path::new("/x/rules/simporter.md")), Mode::Own);
    }
}
