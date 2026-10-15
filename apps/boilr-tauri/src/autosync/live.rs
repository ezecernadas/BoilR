//! The real `Work`: launchers, Steam's shortcut files and the import pipeline.

use std::sync::Mutex;

use boilr::renames::load_rename_map;
use boilr_core::{
    settings::Settings,
    steam::{ensure_steam_stopped, is_steam_running, try_start_steam},
};

use super::{
    detect::{blocked_platforms, current_owned_ids, detect},
    executor::{run_autosync, ExecOutcome, SyncSteps},
    probes::{game_dirs, game_start_dir},
    runner::{CheckOutput, Work},
};
use crate::{
    gather_platform_snapshots, sync_snapshots, try_import_lock, PlatformSnapshot, IMPORT_LOCK,
};

pub struct LiveWork;

impl Work for LiveWork {
    fn load_settings(&self) -> Result<Settings, String> {
        Settings::new().map_err(|err| err.to_string())
    }

    fn check(&self, settings: &Settings) -> Result<CheckOutput, String> {
        let snapshots = gather_platform_snapshots();
        let renames = load_rename_map();
        let current = current_owned_ids(settings)?;
        let result = detect(&snapshots, settings, &renames, &current);

        // Launcher folders are left out: the launcher keeps running after the game ends.
        // Steam library folders are left out too: tools such as Wallpaper Engine run from
        // there all day; Steam's own RunningAppID covers Steam games.
        let mut raw: Vec<String> = snapshots
            .iter()
            .filter(|snapshot| snapshot.enabled)
            .flat_map(|snapshot| {
                snapshot
                    .shortcuts
                    .iter()
                    .flatten()
                    .filter_map(|entry| game_start_dir(&snapshot.code_name, &entry.shortcut))
            })
            .collect();
        raw.extend(xbox_dirs(&snapshots));

        Ok(CheckOutput {
            result,
            game_dirs: game_dirs(raw),
        })
    }

    fn sync(&self, settings: &Settings, restart_steam: bool) -> ExecOutcome {
        let steps = LiveSteps {
            settings: settings.clone(),
        };
        locked(&IMPORT_LOCK, || run_autosync(&steps, restart_steam))
    }
}

struct LiveSteps {
    settings: Settings,
}

impl SyncSteps for LiveSteps {
    type Prepared = Vec<PlatformSnapshot>;

    fn prepare(&self) -> Result<Vec<PlatformSnapshot>, String> {
        checked_snapshots(gather_platform_snapshots)
    }

    fn steam_running(&self) -> bool {
        is_steam_running()
    }

    fn stop_steam(&self) {
        ensure_steam_stopped(&self.settings.steam);
    }

    fn start_steam(&self) -> Result<(), String> {
        try_start_steam(&self.settings.steam).map_err(|err| err.to_string())
    }

    fn import(&self, snapshots: Vec<PlatformSnapshot>) -> Result<(), String> {
        sync_snapshots(self.settings.clone(), snapshots, None, false).map(|_| ())
    }
}

/// Runs `sync` holding the import lock for all of it (launchers, stop, import, start), so
/// a manual import can't rewrite `shortcuts.vdf` meanwhile. A manual import already running
/// fails this sync without touching Steam.
fn locked(lock: &Mutex<()>, sync: impl FnOnce() -> ExecOutcome) -> ExecOutcome {
    match try_import_lock(lock, MANUAL_IMPORTING) {
        Ok(_import) => sync(),
        Err(err) => ExecOutcome {
            import: Err(err),
            steam_start_error: None,
        },
    }
}

const MANUAL_IMPORTING: &str = "another import is running";

/// Reads the launchers again right before importing and refuses when one failed since the
/// check: the import would delete that launcher's games from Steam.
fn checked_snapshots(
    gather: impl FnOnce() -> Vec<PlatformSnapshot>,
) -> Result<Vec<PlatformSnapshot>, String> {
    let snapshots = gather();
    let blocked = blocked_platforms(&snapshots);
    if blocked.is_empty() {
        Ok(snapshots)
    } else {
        Err(format!("Blocked: {}", blocked.join(", ")))
    }
}

/// Xbox shortcuts start `explorer.exe` from the Windows folder, so their games are found
/// by install folder instead.
#[cfg(windows)]
fn xbox_dirs(snapshots: &[PlatformSnapshot]) -> Vec<String> {
    let enabled = snapshots
        .iter()
        .any(|snapshot| snapshot.code_name == "gamepass" && snapshot.enabled);
    if !enabled {
        return Vec::new();
    }
    boilr::platforms::xbox_install_locations().unwrap_or_else(|err| {
        eprintln!("Could not list Xbox install folders: {err}");
        Vec::new()
    })
}

#[cfg(not(windows))]
fn xbox_dirs(_snapshots: &[PlatformSnapshot]) -> Vec<String> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use boilr::platforms::ShortcutToImport;
    use steam_shortcuts_util::Shortcut;

    use super::*;

    fn snapshot(name: &str, error: Option<&str>) -> PlatformSnapshot {
        let shortcut = Shortcut::new("0", "Game", "game.exe", "C:\\Games", "", "", "").to_owned();
        PlatformSnapshot {
            display_name: name.to_string(),
            code_name: name.to_lowercase(),
            enabled: true,
            shortcuts: error.is_none().then(|| {
                vec![ShortcutToImport {
                    shortcut,
                    needs_proton: false,
                    needs_symlinks: false,
                }]
            }),
            error: error.map(str::to_string),
        }
    }

    #[test]
    fn a_platform_that_fails_at_import_time_refuses_the_import() {
        let result =
            checked_snapshots(|| vec![snapshot("GOG", None), snapshot("Epic", Some("timeout"))]);
        assert_eq!(result.map(|s| s.len()), Err("Blocked: Epic".to_string()));
    }

    #[test]
    fn an_import_running_elsewhere_fails_the_sync_without_running_it() {
        let lock = Mutex::new(());
        let _manual = lock.lock().expect("fresh lock");
        let mut ran = false;
        let outcome = locked(&lock, || {
            ran = true;
            ExecOutcome {
                import: Ok(()),
                steam_start_error: None,
            }
        });
        assert!(!ran);
        assert_eq!(
            outcome,
            ExecOutcome {
                import: Err(MANUAL_IMPORTING.to_string()),
                steam_start_error: None
            }
        );
    }

    #[test]
    fn the_sync_holds_the_lock_while_it_runs() {
        let lock = Mutex::new(());
        let outcome = locked(&lock, || {
            assert!(lock.try_lock().is_err(), "held during the sync");
            ExecOutcome {
                import: Ok(()),
                steam_start_error: None,
            }
        });
        assert_eq!(outcome.import, Ok(()));
        assert!(lock.try_lock().is_ok(), "released after the sync");
    }

    #[test]
    fn the_import_gets_the_snapshots_that_were_checked() {
        let result = checked_snapshots(|| vec![snapshot("GOG", None)]);
        assert_eq!(result.map(|s| s.len()), Ok(1));
    }
}
