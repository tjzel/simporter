use std::collections::HashSet;

use crate::config::Config;
use crate::devices::Platform;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentName {
    pub base: String,
    pub index: u32,
}

pub fn is_managed(config: &Config, platform: Platform, name: &str) -> bool {
    parse(config, platform, name).is_some()
}

pub fn parse(config: &Config, platform: Platform, name: &str) -> Option<AgentName> {
    let separator = separator(platform);
    let tokens: Vec<&str> = name
        .split(separator)
        .filter(|token| !token.is_empty())
        .collect();
    let (suffix_at, index) = match tokens.as_slice() {
        [.., suffix, tail] if *suffix == config.agent_suffix => {
            (tokens.len() - 2, tail.parse::<u32>().ok()?)
        }
        [.., suffix] if *suffix == config.agent_suffix => (tokens.len() - 1, 0),
        _ => return None,
    };
    if suffix_at == 0 {
        return None;
    }
    Some(AgentName {
        base: tokens[..suffix_at].join(separator),
        index,
    })
}

pub fn format_name(config: &Config, platform: Platform, base: &str, index: u32) -> String {
    let separator = separator(platform);
    format!("{base}{separator}{}{separator}{index}", config.agent_suffix)
}

pub fn next_free_name(
    config: &Config,
    platform: Platform,
    base: &str,
    taken: &HashSet<String>,
) -> String {
    (1..u32::MAX)
        .map(|index| format_name(config, platform, base, index))
        .find(|candidate| !taken.contains(candidate))
        .expect("agent name space exhausted")
}

pub fn default_base(config: &Config, platform: Platform) -> String {
    match platform {
        Platform::Ios => config.default_device_type.clone(),
        Platform::Android => config.default_avd_base.clone(),
    }
}

fn separator(platform: Platform) -> &'static str {
    match platform {
        Platform::Ios => " ",
        Platform::Android => "_",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Config {
        Config::default()
    }

    #[test]
    fn ios_agent_name_is_managed() {
        let parsed = parse(&config(), Platform::Ios, "iPhone 17 Pro Agent 1").unwrap();
        assert_eq!(
            parsed,
            AgentName {
                base: "iPhone 17 Pro".to_string(),
                index: 1
            }
        );
    }

    #[test]
    fn android_agent_name_is_managed() {
        let parsed = parse(&config(), Platform::Android, "Pixel_9_Agent_2").unwrap();
        assert_eq!(
            parsed,
            AgentName {
                base: "Pixel_9".to_string(),
                index: 2
            }
        );
    }

    #[test]
    fn suffix_without_index_is_managed_as_index_zero() {
        let parsed = parse(&config(), Platform::Ios, "iPhone 17 Pro Agent").unwrap();
        assert_eq!(
            parsed,
            AgentName {
                base: "iPhone 17 Pro".to_string(),
                index: 0
            }
        );
    }

    #[test]
    fn human_owned_names_are_not_managed() {
        let config = config();
        assert!(!is_managed(&config, Platform::Ios, "iPhone 17 Pro"));
        assert!(!is_managed(&config, Platform::Ios, "iPad (A16)"));
        assert!(!is_managed(&config, Platform::Android, "rt_pixel9"));
        assert!(!is_managed(&config, Platform::Ios, "Agent"));
        assert!(!is_managed(&config, Platform::Ios, "Agent 1"));
    }

    #[test]
    fn suffix_must_be_a_whole_token() {
        assert!(!is_managed(&config(), Platform::Ios, "iPhone Agentic 1"));
    }

    #[test]
    fn android_separator_is_not_applied_to_ios() {
        assert!(!is_managed(&config(), Platform::Ios, "Pixel_9_Agent_2"));
    }

    #[test]
    fn next_free_name_skips_taken_names() {
        let taken: HashSet<String> = ["iPhone 17 Pro Agent 1", "iPhone 17 Pro Agent 2"]
            .into_iter()
            .map(String::from)
            .collect();
        let next = next_free_name(&config(), Platform::Ios, "iPhone 17 Pro", &taken);
        assert_eq!(next, "iPhone 17 Pro Agent 3");
    }

    #[test]
    fn next_free_name_starts_at_one() {
        let next = next_free_name(&config(), Platform::Android, "Pixel_9", &HashSet::new());
        assert_eq!(next, "Pixel_9_Agent_1");
    }
}
