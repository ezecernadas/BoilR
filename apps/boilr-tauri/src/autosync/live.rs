//! The real `Work`: launchers, Steam's shortcut files and the import pipeline.

use std::panic::{catch_unwind, AssertUnwindSafe};

use boilr::renames::load_rename_map;
use boilr_core::{
    settings::Settings,
    steam::{ensure_steam_stopped, get_steam_common_folders, try_start_steam},
};

use super::{
    detect::{blocked_platforms, current_owned_ids, detect},
    executor::{run_autosync, ExecOutcome, SyncSteps},
    probes::{game_dirs, game_start_dir},
    runner::{CheckOutput, Work},
};
use crate::{gather_platform_snapshots, sync_snapshots, PlatformSnapshot};

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
        raw.extend(
            get_steam_common_folders(&settings.steam)
                .into_iter()
                .map(|folder| folder.to_string_lossy().into_owned()),
        );
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
        run_autosync(&steps, restart_steam)
    }
}

struct LiveSteps {
    settings: Settings,
}

impl SyncSteps for LiveSteps {
    fn stop_steam(&self) {
        ensure_steam_stopped(&self.settings.steam);
    }

    fn start_steam(&self) -> Result<(), String> {
        try_start_steam(&self.settings.steam).map_err(|err| err.to_string())
    }

    fn import(&self) -> Result<(), String> {
        guarded_import(gather_platform_snapshots, |snapshots| {
            sync_snapshots(self.settings.clone(), snapshots, None, false).map(|_| ())
        })
    }
}

/// Reads the launchers again right before importing and refuses when one failed since the
/// check: the import would delete that launcher's games from Steam. A panic becomes an
/// error, so the executor still starts Steam again.
fn guarded_import(
    gather: impl FnOnce() -> Vec<PlatformSnapshot>,
    import: impl FnOnce(Vec<PlatformSnapshot>) -> Result<(), String>,
) -> Result<(), String> {
    let run = || {
        let snapshots = gather();
        let blocked = blocked_platforms(&snapshots);
        if !blocked.is_empty() {
            return Err(format!("Blocked: {}", blocked.join(", ")));
        }
        import(snapshots)
    };
    catch_unwind(AssertUnwindSafe(run)).unwrap_or_else(|payload| {
        let reason = payload
            .downcast_ref::<&str>()
            .map(|text| text.to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown error".to_string());
        Err(format!("The import stopped unexpectedly: {reason}"))
    })
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
    use std::{cell::RefCell, panic, sync::Once};

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

    const IMPORT_PANIC: &str = "the importer crashed";

    /// Keeps the expected panics of these tests out of the test output. Installed once and
    /// never removed, so tests running in parallel can't race on the global hook; every
    /// other panic still goes to the previous hook.
    fn mute_import_panics() {
        static MUTE: Once = Once::new();
        MUTE.call_once(|| {
            let previous = panic::take_hook();
            panic::set_hook(Box::new(move |info| {
                let message = info
                    .payload()
                    .downcast_ref::<&str>()
                    .copied()
                    .or_else(|| info.payload().downcast_ref::<String>().map(String::as_str));
                if message != Some(IMPORT_PANIC) {
                    previous(info);
                }
            }));
        });
    }

    #[test]
    fn a_platform_that_fails_at_import_time_refuses_the_import() {
        let imported = RefCell::new(false);
        let result = guarded_import(
            || vec![snapshot("GOG", None), snapshot("Epic", Some("timeout"))],
            |_| {
                *imported.borrow_mut() = true;
                Ok(())
            },
        );
        assert_eq!(result, Err("Blocked: Epic".to_string()));
        assert!(!*imported.borrow());
    }

    #[test]
    fn the_import_gets_the_snapshots_that_were_checked() {
        let result = guarded_import(
            || vec![snapshot("GOG", None)],
            |snapshots| {
                assert_eq!(snapshots.len(), 1);
                Ok(())
            },
        );
        assert_eq!(result, Ok(()));
    }

    #[test]
    fn a_panicking_import_becomes_an_error() {
        mute_import_panics();
        let result = guarded_import(|| vec![snapshot("GOG", None)], |_| panic!("{IMPORT_PANIC}"));
        let err = result.expect_err("a panic is an error");
        assert!(err.contains(IMPORT_PANIC), "{err}");
    }

    struct Steps {
        log: RefCell<Vec<&'static str>>,
        blocked: bool,
    }

    impl SyncSteps for Steps {
        fn stop_steam(&self) {
            self.log.borrow_mut().push("stop");
        }
        fn start_steam(&self) -> Result<(), String> {
            self.log.borrow_mut().push("start");
            Ok(())
        }
        fn import(&self) -> Result<(), String> {
            let error = self.blocked.then_some("unreachable");
            guarded_import(
                || vec![snapshot("Epic", error)],
                |_| panic!("{IMPORT_PANIC}"),
            )
        }
    }

    #[test]
    fn steam_is_started_again_after_a_blocked_or_panicking_import() {
        mute_import_panics();
        for blocked in [true, false] {
            let steps = Steps {
                log: RefCell::new(Vec::new()),
                blocked,
            };
            let outcome = run_autosync(&steps, true);
            assert!(outcome.import.is_err());
            assert_eq!(*steps.log.borrow(), vec!["stop", "start"]);
        }
    }
}
