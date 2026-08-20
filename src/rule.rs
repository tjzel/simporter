use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;

use crate::cli::{OutputArgs, RuleArgs};
use crate::exit;

pub const TEXT: &str = include_str!("../agent-rule.md");

const BEGIN: &str = "<!-- BEGIN simporter (managed by `simporter rule --install`) -->";
const END: &str = "<!-- END simporter -->";

const HARNESSES: &[Harness] = &[
    Harness {
        name: "claude code",
        home: ".claude",
        file: "rules/simporter.md",
        mode: Mode::Own,
    },
    Harness {
        name: "codex",
        home: ".codex",
        file: "AGENTS.md",
        mode: Mode::Block,
    },
    Harness {
        name: "kimi code",
        home: ".kimi-code",
        file: "AGENTS.md",
        mode: Mode::Block,
    },
    Harness {
        name: "gemini cli",
        home: ".gemini",
        file: "GEMINI.md",
        mode: Mode::Block,
    },
    Harness {
        name: "cursor",
        home: ".cursor",
        file: "rules/simporter.md",
        mode: Mode::Own,
    },
    Harness {
        name: "opencode",
        home: ".config/opencode",
        file: "AGENTS.md",
        mode: Mode::Block,
    },
];

pub fn run(args: &RuleArgs) -> Result<i32> {
    if !args.install && !args.dry_run {
        print!("{TEXT}");
        return Ok(exit::OK);
    }
    let plan = plan(&args.target)?;
    for entry in &plan {
        if args.install && entry.action != Action::Skipped {
            apply(entry).with_context(|| format!("failed to write {}", entry.path.display()))?;
        }
    }
    report(args.output, &plan, args.install);
    Ok(exit::OK)
}

fn plan(extra: &[PathBuf]) -> Result<Vec<Entry>> {
    let mut plan = Vec::new();
    if !extra.is_empty() {
        for path in extra {
            plan.push(entry_for("custom", path.clone(), infer_mode(path))?);
        }
        return Ok(plan);
    }
    let home = dirs::home_dir().context("could not determine the home directory")?;
    for harness in HARNESSES {
        let root = home.join(harness.home);
        let path = root.join(harness.file);
        if !root.is_dir() {
            plan.push(Entry {
                harness: harness.name,
                path,
                mode: harness.mode,
                action: Action::Skipped,
                detail: "harness not installed",
            });
            continue;
        }
        plan.push(entry_for(harness.name, path, harness.mode)?);
    }
    Ok(plan)
}

fn entry_for(harness: &'static str, path: PathBuf, mode: Mode) -> Result<Entry> {
    let existing = if path.exists() {
        Some(
            std::fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?,
        )
    } else {
        None
    };
    let wanted = render(mode);
    let action = match &existing {
        None => Action::Created,
        Some(current) if *current == wanted && mode == Mode::Own => Action::Unchanged,
        Some(current) if mode == Mode::Block && splice(current) == *current => Action::Unchanged,
        Some(_) => Action::Updated,
    };
    let detail = match mode {
        Mode::Own => "dedicated rule file",
        Mode::Block => "managed block inside a shared instructions file",
    };
    Ok(Entry {
        harness,
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
        Mode::Own => render(Mode::Own),
        Mode::Block => match std::fs::read_to_string(&entry.path) {
            Ok(current) => splice(&current),
            Err(_) => render(Mode::Block),
        },
    };
    crate::state::write_atomic(&entry.path, &body)
}

fn splice(current: &str) -> String {
    let block = render(Mode::Block);
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

fn render(mode: Mode) -> String {
    match mode {
        Mode::Own => TEXT.to_string(),
        Mode::Block => format!("{BEGIN}\n\n{}\n{END}\n", TEXT.trim_end()),
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
            "{:<10} {:<12} {}",
            entry.action.as_str(),
            entry.harness,
            entry.path.display()
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
}

#[derive(Debug)]
struct Entry {
    harness: &'static str,
    path: PathBuf,
    mode: Mode,
    action: Action,
    detail: &'static str,
}

#[derive(Debug, Serialize)]
struct EntryPayload {
    harness: &'static str,
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
        let lowered = TEXT.to_lowercase();
        for vendor in ["claude", "codex", "kimi", "gemini", "cursor"] {
            assert!(!lowered.contains(vendor), "the rule mentions {vendor}");
        }
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
