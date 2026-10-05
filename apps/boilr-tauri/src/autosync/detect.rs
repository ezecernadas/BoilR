//! Whether the games BoilR would import differ from the BoilR games already in Steam.
//! `plan_sync` can't answer this: it lists every BoilR shortcut as a removal, because a
//! sync removes them all and re-adds the current set.

use std::collections::{BTreeSet, HashMap};

use boilr_core::{
    settings::Settings,
    steam::{get_shortcuts_for_user, get_shortcuts_paths},
    sync::IsBoilRShortcut,
};

use super::machine::{ChangeSet, CheckResult};
use crate::{prepare_additions, PlatformSnapshot};

/// Display names of the enabled platforms whose snapshot has an error or no data. A sync
/// removes every BoilR shortcut and re-adds what answered, so importing while one of these
/// is missing would delete its games from Steam. Disabled platforms are never listed, and a
/// platform that answered without an error is not listed even when it found no games.
pub fn blocked_platforms(snapshots: &[PlatformSnapshot]) -> Vec<String> {
    snapshots
        .iter()
        .filter(|snapshot| {
            snapshot.enabled && (snapshot.error.is_some() || snapshot.shortcuts.is_none())
        })
        .map(|snapshot| snapshot.display_name.clone())
        .collect()
}

/// Compares the app ids a sync would write (`desired`) with the BoilR-owned ids in Steam
/// (`current`).
pub fn detect(
    snapshots: &[PlatformSnapshot],
    settings: &Settings,
    renames: &HashMap<u32, String>,
    current: &BTreeSet<u32>,
) -> CheckResult {
    let blocked = blocked_platforms(snapshots);
    if !blocked.is_empty() {
        return CheckResult::Blocked { platforms: blocked };
    }

    // With no launcher data `perform_full_sync` returns before touching Steam, so a
    // difference here could never be resolved.
    if !snapshots
        .iter()
        .any(|snapshot| snapshot.enabled && snapshot.shortcuts.is_some())
    {
        return CheckResult::NoChange;
    }

    let (_, desired) = prepare_additions(snapshots, settings, renames);
    let desired: BTreeSet<u32> = desired.into_iter().collect();
    let change = ChangeSet {
        added: desired.difference(current).copied().collect(),
        removed: current.difference(&desired).copied().collect(),
    };
    if change.is_empty() {
        CheckResult::NoChange
    } else {
        CheckResult::Changed(change)
    }
}

/// App ids of the BoilR-owned shortcuts in `shortcuts.vdf`, over every Steam user. An
/// unreadable file is an error: the check can't tell what is in Steam.
pub fn current_owned_ids(settings: &Settings) -> Result<BTreeSet<u32>, String> {
    let users = get_shortcuts_paths(&settings.steam).map_err(|err| err.to_string())?;
    let mut ids = BTreeSet::new();
    for user in users {
        let info = get_shortcuts_for_user(&user).map_err(|err| err.to_string())?;
        ids.extend(
            info.shortcuts
                .iter()
                .filter(|shortcut| shortcut.is_boilr_shortcut())
                .map(|shortcut| shortcut.app_id),
        );
    }
    Ok(ids)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    #![allow(clippy::expect_used)]

    use super::*;
    use crate::autosync::machine::ChangeSet;
    use boilr::platforms::ShortcutToImport;
    use steam_shortcuts_util::Shortcut;

    fn game(app_id: u32, name: &str) -> ShortcutToImport {
        let mut shortcut = Shortcut::new("0", name, "game.exe", "C:\\Games", "", "", "").to_owned();
        shortcut.app_id = app_id;
        ShortcutToImport {
            shortcut,
            needs_proton: false,
            needs_symlinks: false,
        }
    }

    fn platform(
        name: &str,
        enabled: bool,
        games: Option<Vec<ShortcutToImport>>,
        error: Option<&str>,
    ) -> PlatformSnapshot {
        PlatformSnapshot {
            display_name: name.to_string(),
            code_name: name.to_lowercase(),
            enabled,
            shortcuts: games,
            error: error.map(str::to_string),
        }
    }

    /// Built from the embedded defaults, never from the user's config file.
    fn settings() -> Settings {
        let mut settings: Settings = toml::from_str(include_str!(
            "../../../../crates/boilr-core/src/defaultconfig.toml"
        ))
        .expect("default config fills Settings");
        settings.blacklisted_games.clear();
        settings
    }

    fn ids(values: &[u32]) -> BTreeSet<u32> {
        values.iter().copied().collect()
    }

    fn run(snapshots: &[PlatformSnapshot], settings: &Settings, current: &[u32]) -> CheckResult {
        detect(snapshots, settings, &HashMap::new(), &ids(current))
    }

    #[test]
    fn an_added_game_is_a_change() {
        let snapshots = [platform(
            "Epic",
            true,
            Some(vec![game(1, "A"), game(2, "B")]),
            None,
        )];
        assert_eq!(
            run(&snapshots, &settings(), &[1]),
            CheckResult::Changed(ChangeSet {
                added: ids(&[2]),
                removed: ids(&[])
            })
        );
    }

    #[test]
    fn a_removed_game_is_a_change() {
        let snapshots = [platform("Epic", true, Some(vec![game(1, "A")]), None)];
        assert_eq!(
            run(&snapshots, &settings(), &[1, 3]),
            CheckResult::Changed(ChangeSet {
                added: ids(&[]),
                removed: ids(&[3])
            })
        );
    }

    #[test]
    fn the_same_games_are_no_change() {
        let snapshots = [platform("Epic", true, Some(vec![game(1, "A")]), None)];
        assert_eq!(run(&snapshots, &settings(), &[1]), CheckResult::NoChange);
    }

    #[test]
    fn a_failing_enabled_platform_blocks_the_check() {
        let snapshots = [
            platform("Epic", true, Some(vec![game(1, "A")]), None),
            platform("GOG", true, None, Some("database locked")),
        ];
        assert_eq!(
            run(&snapshots, &settings(), &[]),
            CheckResult::Blocked {
                platforms: vec!["GOG".to_string()]
            }
        );
    }

    #[test]
    fn a_disabled_platform_does_not_block() {
        let snapshots = [
            platform("Epic", true, Some(vec![game(1, "A")]), None),
            platform("GOG", false, None, None),
        ];
        assert_eq!(run(&snapshots, &settings(), &[1]), CheckResult::NoChange);
    }

    #[test]
    fn a_blacklisted_game_is_not_counted() {
        let mut settings = settings();
        settings.blacklisted_games = vec![2];
        let snapshots = [platform(
            "Epic",
            true,
            Some(vec![game(1, "A"), game(2, "B")]),
            None,
        )];
        assert_eq!(run(&snapshots, &settings, &[1]), CheckResult::NoChange);
    }

    #[test]
    fn no_platform_data_is_no_change() {
        let snapshots = [platform("Epic", false, None, None)];
        assert_eq!(run(&snapshots, &settings(), &[5, 6]), CheckResult::NoChange);
    }

    #[test]
    fn blocked_platforms_lists_an_enabled_platform_with_an_error() {
        let snapshots = [platform("GOG", true, None, Some("database locked"))];
        assert_eq!(blocked_platforms(&snapshots), vec!["GOG".to_string()]);
    }

    #[test]
    fn blocked_platforms_lists_an_enabled_platform_with_an_error_and_data() {
        let snapshots = [platform(
            "GOG",
            true,
            Some(vec![game(1, "A")]),
            Some("partial read"),
        )];
        assert_eq!(blocked_platforms(&snapshots), vec!["GOG".to_string()]);
    }

    #[test]
    fn blocked_platforms_lists_an_enabled_platform_without_data() {
        let snapshots = [platform("GOG", true, None, None)];
        assert_eq!(blocked_platforms(&snapshots), vec!["GOG".to_string()]);
    }

    #[test]
    fn blocked_platforms_skips_a_disabled_platform_with_an_error() {
        let snapshots = [platform("GOG", false, None, Some("database locked"))];
        assert!(blocked_platforms(&snapshots).is_empty());
    }

    #[test]
    fn blocked_platforms_skips_an_enabled_platform_that_answered() {
        let snapshots = [platform("Epic", true, Some(vec![game(1, "A")]), None)];
        assert!(blocked_platforms(&snapshots).is_empty());
    }
}
