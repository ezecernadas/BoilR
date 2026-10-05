//! Runs one autosync import. Steam is stopped and started here when the state machine
//! asks for it, not according to the `stop_steam`/`start_steam` settings, which keep
//! governing manual imports.

use std::panic::{catch_unwind, AssertUnwindSafe};

pub trait SyncSteps {
    /// What the import needs from the launchers.
    type Prepared;
    /// Reads the launchers and refuses when the import would delete games.
    fn prepare(&self) -> Result<Self::Prepared, String>;
    fn steam_running(&self) -> bool;
    fn stop_steam(&self);
    fn start_steam(&self) -> Result<(), String>;
    fn import(&self, prepared: Self::Prepared) -> Result<(), String>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecOutcome {
    pub import: Result<(), String>,
    /// Steam was stopped for the import and could not be started again.
    pub steam_start_error: Option<String>,
}

/// An import without a restart found Steam running: writing its files now would race
/// with Steam, so the import waits for the user's Sync now.
pub const STEAM_STARTED: &str = "Steam was started before the import";

/// 1. Reads the launchers; when that fails Steam is never touched.
/// 2. Without a restart, makes sure Steam is still closed.
/// 3. Stops Steam (with a restart) and imports.
/// 4. Starts Steam again whatever step 3 returned, panics included.
pub fn run_autosync<S: SyncSteps>(steps: &S, restart_steam: bool) -> ExecOutcome {
    let refused = |import| ExecOutcome {
        import: Err(import),
        steam_start_error: None,
    };
    let prepared = match catch(|| steps.prepare()) {
        Ok(prepared) => prepared,
        Err(err) => return refused(err),
    };
    if !restart_steam {
        match catch(|| Ok(steps.steam_running())) {
            Ok(false) => {}
            Ok(true) => return refused(STEAM_STARTED.to_string()),
            Err(err) => return refused(err),
        }
    }
    let import = catch(|| {
        if restart_steam {
            steps.stop_steam();
        }
        steps.import(prepared)
    });
    let steam_start_error = if restart_steam {
        catch(|| steps.start_steam()).err()
    } else {
        None
    };
    ExecOutcome {
        import,
        steam_start_error,
    }
}

/// Turns a panic into an error, so Steam is still started again after one.
fn catch<T>(step: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    catch_unwind(AssertUnwindSafe(step)).unwrap_or_else(|payload| {
        let reason = payload
            .downcast_ref::<&str>()
            .map(|text| text.to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown error".to_string());
        Err(format!("The import stopped unexpectedly: {reason}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, panic, sync::Once};

    const STEP_PANIC: &str = "a step crashed";

    /// Keeps the expected panics of these tests out of the test output. Installed once and
    /// never removed, so tests running in parallel can't race on the global hook; every
    /// other panic still goes to the previous hook.
    fn mute_step_panics() {
        static MUTE: Once = Once::new();
        MUTE.call_once(|| {
            let previous = panic::take_hook();
            panic::set_hook(Box::new(move |info| {
                let message = info
                    .payload()
                    .downcast_ref::<&str>()
                    .copied()
                    .or_else(|| info.payload().downcast_ref::<String>().map(String::as_str));
                if message != Some(STEP_PANIC) {
                    previous(info);
                }
            }));
        });
    }

    struct FakeSteps {
        log: RefCell<Vec<&'static str>>,
        prepare: Result<(), String>,
        import: Result<(), String>,
        start: Result<(), String>,
        steam_running: bool,
        panics_in: Option<&'static str>,
    }

    impl FakeSteps {
        fn new(import: Result<(), String>, start: Result<(), String>) -> Self {
            Self {
                log: RefCell::new(Vec::new()),
                prepare: Ok(()),
                import,
                start,
                steam_running: true,
                panics_in: None,
            }
        }

        fn step(&self, name: &'static str) {
            self.log.borrow_mut().push(name);
            if self.panics_in == Some(name) {
                panic!("{STEP_PANIC}");
            }
        }

        fn log(&self) -> Vec<&'static str> {
            self.log.borrow().clone()
        }
    }

    impl SyncSteps for FakeSteps {
        type Prepared = ();
        fn prepare(&self) -> Result<(), String> {
            self.step("prepare");
            self.prepare.clone()
        }
        fn steam_running(&self) -> bool {
            self.step("probe");
            self.steam_running
        }
        fn stop_steam(&self) {
            self.step("stop");
        }
        fn start_steam(&self) -> Result<(), String> {
            self.step("start");
            self.start.clone()
        }
        fn import(&self, _prepared: ()) -> Result<(), String> {
            self.step("import");
            self.import.clone()
        }
    }

    #[test]
    fn a_restart_reads_the_launchers_stops_steam_imports_then_starts_it() {
        let steps = FakeSteps::new(Ok(()), Ok(()));
        let outcome = run_autosync(&steps, true);
        assert_eq!(steps.log(), vec!["prepare", "stop", "import", "start"]);
        assert_eq!(
            outcome,
            ExecOutcome {
                import: Ok(()),
                steam_start_error: None
            }
        );
    }

    #[test]
    fn steam_is_started_again_when_the_import_fails() {
        let steps = FakeSteps::new(Err("disk full".to_string()), Ok(()));
        let outcome = run_autosync(&steps, true);
        assert_eq!(steps.log(), vec!["prepare", "stop", "import", "start"]);
        assert_eq!(
            outcome,
            ExecOutcome {
                import: Err("disk full".to_string()),
                steam_start_error: None
            }
        );
    }

    #[test]
    fn a_failed_import_and_a_failed_start_are_both_reported() {
        let steps = FakeSteps::new(
            Err("disk full".to_string()),
            Err("Steam was not found".to_string()),
        );
        let outcome = run_autosync(&steps, true);
        assert_eq!(outcome.import, Err("disk full".to_string()));
        assert_eq!(
            outcome.steam_start_error,
            Some("Steam was not found".to_string())
        );
    }

    #[test]
    fn without_a_restart_steam_is_only_checked_to_be_closed() {
        let mut steps = FakeSteps::new(Ok(()), Ok(()));
        steps.steam_running = false;
        let outcome = run_autosync(&steps, false);
        assert_eq!(steps.log(), vec!["prepare", "probe", "import"]);
        assert_eq!(outcome.import, Ok(()));
    }

    #[test]
    fn without_a_restart_a_running_steam_refuses_the_import() {
        let steps = FakeSteps::new(Ok(()), Ok(()));
        let outcome = run_autosync(&steps, false);
        assert_eq!(steps.log(), vec!["prepare", "probe"]);
        assert_eq!(outcome.import, Err(STEAM_STARTED.to_string()));
        assert_eq!(outcome.steam_start_error, None);
    }

    #[test]
    fn a_failed_start_is_reported_apart_from_the_import() {
        let steps = FakeSteps::new(
            Ok(()),
            Err("Steam was not found at C:\\Steam\\steam.exe".to_string()),
        );
        let outcome = run_autosync(&steps, true);
        assert_eq!(outcome.import, Ok(()));
        assert_eq!(
            outcome.steam_start_error,
            Some("Steam was not found at C:\\Steam\\steam.exe".to_string())
        );
    }

    /// A launcher that fails at the deadline must not cost the user a Steam restart.
    #[test]
    fn a_refused_import_never_touches_steam() {
        for restart_steam in [true, false] {
            let mut steps = FakeSteps::new(Ok(()), Ok(()));
            steps.prepare = Err("Blocked: Epic".to_string());
            let outcome = run_autosync(&steps, restart_steam);
            assert_eq!(steps.log(), vec!["prepare"]);
            assert_eq!(
                outcome,
                ExecOutcome {
                    import: Err("Blocked: Epic".to_string()),
                    steam_start_error: None
                }
            );
        }
    }

    #[test]
    fn a_panic_while_stopping_steam_still_starts_it() {
        mute_step_panics();
        let mut steps = FakeSteps::new(Ok(()), Ok(()));
        steps.panics_in = Some("stop");
        let outcome = run_autosync(&steps, true);
        assert_eq!(steps.log(), vec!["prepare", "stop", "start"]);
        let err = outcome.import.expect_err("a panic is an error");
        assert!(err.contains(STEP_PANIC), "{err}");
    }

    #[test]
    fn a_panicking_import_still_starts_steam() {
        mute_step_panics();
        let mut steps = FakeSteps::new(Ok(()), Ok(()));
        steps.panics_in = Some("import");
        let outcome = run_autosync(&steps, true);
        assert_eq!(steps.log(), vec!["prepare", "stop", "import", "start"]);
        assert!(outcome.import.is_err());
    }

    #[test]
    fn a_panic_while_reading_the_launchers_touches_nothing() {
        mute_step_panics();
        let mut steps = FakeSteps::new(Ok(()), Ok(()));
        steps.panics_in = Some("prepare");
        let outcome = run_autosync(&steps, true);
        assert_eq!(steps.log(), vec!["prepare"]);
        assert!(outcome.import.is_err());
        assert_eq!(outcome.steam_start_error, None);
    }

    #[test]
    fn a_panic_while_starting_steam_is_reported() {
        mute_step_panics();
        let mut steps = FakeSteps::new(Ok(()), Ok(()));
        steps.panics_in = Some("start");
        let outcome = run_autosync(&steps, true);
        assert_eq!(outcome.import, Ok(()));
        assert!(outcome.steam_start_error.is_some());
    }
}
