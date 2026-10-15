use crate::{
    autosync::AutosyncSettings, config::get_config_file, steam::SteamSettings,
    steamgriddb::SteamGridDbSettings,
};

use config::{Config, ConfigError, Environment, File};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, env};

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Settings {
    pub debug: bool,
    pub config_version: Option<usize>,
    pub blacklisted_games: Vec<u32>,
    pub steamgrid_db: SteamGridDbSettings,
    pub steam: SteamSettings,
    pub autosync: AutosyncSettings,
}

impl Settings {
    pub fn new() -> Result<Self, ConfigError> {
        let default_str = include_str!("defaultconfig.toml");
        let config_file = get_config_file();
        let config_file = config_file.to_string_lossy();
        let env = env::var("RUN_MODE").unwrap_or_else(|_| "development".into());

        let config = Config::builder()
            .add_source(File::from_str(default_str, config::FileFormat::Toml))
            // Start off by merging in the "default" configuration file
            .add_source(File::with_name(config_file.as_ref()).required(false))
            // Add in the current environment file
            // Default to 'development' env
            // Note that this file is _optional_
            .add_source(File::with_name(&format!("config/{env}")).required(false))
            // Add in a local configuration file
            // This file shouldn't be checked in to git
            .add_source(File::with_name("local.toml").required(false))
            // Add in settings from the environment (with a prefix of STEAMSYNC)
            // Eg.. `STEAMSYNC_DEBUG=1 ./target/app` would set the `debug` key
            .add_source(Environment::with_prefix("boilr").separator("-"))
            .build()?;
        let mut settings = config.try_deserialize::<Settings>()?;
        sanitize_auth_key(&mut settings);
        Ok(settings)
    }
}

pub fn load_setting_sections() -> eyre::Result<HashMap<String, String>> {
    let content = std::fs::read_to_string(get_config_file())?;
    Ok(parse_setting_sections(&content))
}

/// Splits a config file into its `[section]` bodies, leaving out the sections that belong
/// to `Settings` itself, so only platform sections remain.
fn parse_setting_sections(content: &str) -> HashMap<String, String> {
    let mut result = HashMap::new();
    let mut current_section_lines: Vec<String> = vec![];
    let mut current_section_name: Option<String> = Option::None;
    for line in content.lines() {
        if line.starts_with('[') && line.ends_with(']') {
            add_sections(&current_section_name, &current_section_lines, &mut result);
            current_section_name = line.get(1..line.len() - 1).map(|s| s.to_string());
            current_section_lines.clear();
        } else {
            current_section_lines.push(line.to_string());
        }
    }
    add_sections(&current_section_name, &current_section_lines, &mut result);

    let blacklisted_sections = ["steamgrid_db", "steam", "autosync"];
    for section in blacklisted_sections {
        let _ = result.remove(section);
    }
    result
}

/// Saves the settings followed by one `[code_name]` section per platform, given as
/// `(code_name, serialized settings)` pairs.
pub fn save_settings(
    settings: &Settings,
    platform_sections: &[(String, String)],
) -> eyre::Result<()> {
    let mut toml = toml::to_string(&settings)?;

    for (code_name, platform_string) in platform_sections {
        let section_name = format!("[{code_name}]");
        toml.push('\n');
        toml.push_str(section_name.as_str());
        toml.push('\n');
        toml.push_str(platform_string.as_str());
    }

    let config_path = crate::config::get_config_file();
    if let Some(folder) = config_path.parent() {
        std::fs::create_dir_all(folder)?;
    }
    std::fs::write(config_path, toml)?;
    Ok(())
}

fn add_sections(
    current_section_name: &Option<String>,
    current_section_lines: &Vec<String>,
    result: &mut HashMap<String, String>,
) {
    if let Some(old_section_name) = current_section_name {
        let mut section_string = String::new();
        for line in current_section_lines {
            section_string.push_str(line);
            section_string.push('\n');
        }
        result.insert(old_section_name.to_string(), section_string);
    }
}

fn sanitize_auth_key(result: &mut Settings) {
    if let Some(auth_key) = result.steamgrid_db.auth_key.as_ref() {
        if auth_key == "Write your authentication key between these quotes" {
            result.steamgrid_db.auth_key = None;
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    #![allow(clippy::expect_used)]

    use super::*;
    use crate::autosync::AutosyncSettings;

    fn default_settings() -> Settings {
        Config::builder()
            .add_source(File::from_str(
                include_str!("defaultconfig.toml"),
                config::FileFormat::Toml,
            ))
            .build()
            .expect("default config parses")
            .try_deserialize()
            .expect("default config fills Settings")
    }

    #[test]
    fn default_config_loads_with_autosync_disabled() {
        let settings = default_settings();
        assert!(!settings.autosync.enabled, "autosync is opt-in");
        assert_eq!(settings.autosync, AutosyncSettings::default());
    }

    #[test]
    fn settings_sections_are_not_platform_sections() {
        let sections = parse_setting_sections(
            "debug = false\n[steam]\nstop_steam = false\n[autosync]\nenabled = true\n[epic_games]\nenabled = true\n",
        );
        assert!(!sections.contains_key("autosync"));
        assert!(!sections.contains_key("steam"));
        assert_eq!(
            sections.get("epic_games").map(String::as_str),
            Some("enabled = true\n")
        );
    }
}
