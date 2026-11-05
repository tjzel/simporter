use std::io::Read;

use serde::{Deserialize, Serialize};

use crate::exit;

const PREFIX_WORDS: &[&str] = &[
    "env",
    "command",
    "exec",
    "time",
    "nice",
    "nohup",
    "caffeinate",
    "sudo",
];

pub fn run() -> i32 {
    let mut raw = String::new();
    if std::io::stdin().read_to_string(&mut raw).is_err() {
        return exit::OK;
    }
    let Ok(payload) = serde_json::from_str::<Payload>(&raw) else {
        return exit::OK;
    };
    if payload.tool_name != "Bash" {
        return exit::OK;
    }
    if let Some(segment) = unwrapped_heavy(&payload.tool_input.command) {
        let decision = Decision {
            hook_specific_output: HookOutput {
                hook_event_name: "PreToolUse",
                permission_decision: "deny",
                permission_decision_reason: reason(&segment),
            },
        };
        if let Ok(body) = serde_json::to_string(&decision) {
            println!("{body}");
        }
    }
    exit::OK
}

fn reason(segment: &str) -> String {
    format!(
        "`{segment}` is heavy work run outside the simporter heavy-work queue. Several agents share \
         this machine, and too many heavy jobs at once stall it. Run it as \
         `simporter heavy -- {segment}` (add `--weight 2` for two simulators at once). It waits for \
         a free slot; `simporter status` shows the queue."
    )
}

fn unwrapped_heavy(command: &str) -> Option<String> {
    segments(command)
        .into_iter()
        .find(|words| is_heavy(strip_prefix(words)))
        .map(|words| words.join(" "))
}

fn segments(command: &str) -> Vec<Vec<String>> {
    let mut splitter = Splitter::default();
    let mut chars = command.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                splitter.in_word = true;
                for quoted in chars.by_ref() {
                    if quoted == '\'' {
                        break;
                    }
                    splitter.word.push(quoted);
                }
            }
            '"' => {
                splitter.in_word = true;
                while let Some(quoted) = chars.next() {
                    match quoted {
                        '"' => break,
                        '\\' => splitter.word.extend(chars.next()),
                        _ => splitter.word.push(quoted),
                    }
                }
            }
            '\\' => {
                splitter.in_word = true;
                splitter
                    .word
                    .extend(chars.next().filter(|next| *next != '\n'));
            }
            '#' if !splitter.in_word => while chars.next_if(|next| *next != '\n').is_some() {},
            ';' | '&' | '|' | '(' | ')' | '\n' => splitter.end_segment(),
            c if c.is_whitespace() => splitter.end_word(),
            c => {
                splitter.in_word = true;
                splitter.word.push(c);
            }
        }
    }
    splitter.end_segment();
    splitter.segments
}

fn strip_prefix(words: &[String]) -> &[String] {
    let mut index = 0;
    while let Some(word) = words.get(index) {
        let assignment = word
            .split_once('=')
            .is_some_and(|(name, _)| is_variable_name(name));
        let prefix = PREFIX_WORDS.contains(&base_name(word));
        let flag_after_prefix = index > 0 && word.starts_with('-');
        if !(assignment || prefix || flag_after_prefix) {
            break;
        }
        index += 1;
    }
    &words[index..]
}

fn is_heavy(words: &[String]) -> bool {
    let Some((program, rest)) = words.split_first() else {
        return false;
    };
    let rest: Vec<&str> = rest.iter().map(String::as_str).collect();
    if only_prints_usage(&rest) {
        return false;
    }
    if runs_a_react_native_build(words) {
        return true;
    }
    match base_name(program) {
        "xcodebuild" | "gradlew" | "gradle" => true,
        "cargo" => matches!(
            rest.iter().find(|arg| !arg.starts_with(['-', '+'])),
            Some(&("build" | "b" | "test" | "t" | "bench" | "install"))
        ),
        "swift" => rest.first() == Some(&"build") && builds_release(&rest),
        "pod" => rest.first() == Some(&"install"),
        "xcrun" => rest.starts_with(&["simctl", "boot"]),
        _ => false,
    }
}

fn only_prints_usage(args: &[&str]) -> bool {
    args.iter()
        .take_while(|arg| **arg != "--")
        .any(|arg| matches!(*arg, "--help" | "-h" | "--version" | "-version"))
}

fn runs_a_react_native_build(words: &[String]) -> bool {
    words.windows(2).any(|pair| {
        let (tool, action) = (base_name(&pair[0]), pair[1].as_str());
        (tool == "react-native" && matches!(action, "run-ios" | "run-android"))
            || (tool == "expo" && matches!(action, "run:ios" | "run:android"))
    })
}

fn builds_release(args: &[&str]) -> bool {
    args.windows(2)
        .any(|pair| matches!(pair[0], "-c" | "--configuration") && pair[1] == "release")
        || args
            .iter()
            .any(|arg| matches!(*arg, "-crelease" | "--configuration=release"))
}

fn base_name(word: &str) -> &str {
    word.rsplit('/').next().unwrap_or(word)
}

fn is_variable_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[derive(Default)]
struct Splitter {
    segments: Vec<Vec<String>>,
    words: Vec<String>,
    word: String,
    in_word: bool,
}

impl Splitter {
    fn end_word(&mut self) {
        if self.in_word {
            self.words.push(std::mem::take(&mut self.word));
            self.in_word = false;
        }
    }

    fn end_segment(&mut self) {
        self.end_word();
        if !self.words.is_empty() {
            self.segments.push(std::mem::take(&mut self.words));
        }
    }
}

#[derive(Deserialize)]
struct Payload {
    #[serde(default)]
    tool_name: String,
    #[serde(default)]
    tool_input: ToolInput,
}

#[derive(Default, Deserialize)]
struct ToolInput {
    #[serde(default)]
    command: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Decision {
    hook_specific_output: HookOutput,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HookOutput {
    hook_event_name: &'static str,
    permission_decision: &'static str,
    permission_decision_reason: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flagged(command: &str) -> Option<String> {
        unwrapped_heavy(command)
    }

    #[test]
    fn plain_heavy_commands_are_flagged() {
        for command in [
            "xcodebuild -workspace App.xcworkspace -scheme App build",
            "./gradlew assembleDebug",
            "cd android && ./gradlew assembleDebug",
            "cargo build --release",
            "cargo +nightly test",
            "swift build -c release",
            "swift build --configuration release",
            "pod install",
            "xcrun simctl boot 1234-ABCD",
            "npx react-native run-ios --simulator 'iPhone 17 Pro Agent 1'",
            "yarn expo run:android",
            "RCT_NO_LAUNCH_PACKAGER=1 xcodebuild build",
            "time nice -n10 cargo build",
            "/usr/bin/xcodebuild build",
            "cargo build 2>&1 | tail -20",
            "echo start; cargo test",
            "cargo test -- --help",
        ] {
            assert!(flagged(command).is_some(), "{command} was not flagged");
        }
    }

    #[test]
    fn wrapped_and_light_commands_pass() {
        for command in [
            "simporter heavy -- xcodebuild build",
            "simporter heavy --weight 2 --label sims -- sh -c 'xcrun simctl boot A && xcrun simctl boot B'",
            "SIMPORTER_OWNER=me simporter heavy -- cargo build",
            "cargo fmt --check",
            "cargo clippy --all-targets",
            "swift build",
            "pod repo update",
            "xcrun simctl list -j devices",
            "echo 'cargo build'",
            "git commit -m \"xcodebuild; cargo build\"",
            "ls # cargo build",
            "grep -rn xcodebuild src",
            "cargo build --help",
            "cargo test -h",
            "xcodebuild -version",
            "./gradlew --version",
            "pod install --help",
            "npx react-native run-ios --help",
        ] {
            assert_eq!(flagged(command), None, "{command} was flagged");
        }
    }

    #[test]
    fn only_the_unwrapped_part_of_a_chain_is_reported() {
        let found = flagged("simporter heavy -- cargo build && cargo test --lib").unwrap();
        assert_eq!(found, "cargo test --lib");
    }

    #[test]
    fn the_reason_suggests_the_wrapped_command() {
        assert!(reason("pod install").contains("`simporter heavy -- pod install`"));
    }

    #[test]
    fn segments_respect_quotes_and_escapes() {
        assert_eq!(
            segments("a 'b c' \"d \\\" e\" f\\ g; h"),
            vec![
                vec!["a", "b c", "d \" e", "f g"]
                    .into_iter()
                    .map(String::from)
                    .collect::<Vec<_>>(),
                vec!["h".to_string()],
            ]
        );
    }
}
