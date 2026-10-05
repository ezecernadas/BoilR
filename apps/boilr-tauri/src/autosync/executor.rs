//! Runs one autosync import. Steam is stopped and started here when the state machine
//! asks for it, not according to the `stop_steam`/`start_steam` settings, which keep
//! governing manual imports.

pub trait SyncSteps {
    fn stop_steam(&self);
    fn start_steam(&self) -> Result<(), String>;
    fn import(&self) -> Result<(), String>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecOutcome {
    pub import: Result<(), String>,
    /// Steam was stopped for the import and could not be started again.
    pub steam_start_error: Option<String>,
}

pub fn run_autosync(steps: &dyn SyncSteps, restart_steam: bool) -> ExecOutcome {
    if restart_steam {
        steps.stop_steam();
    }
    let import = steps.import();
    // Whatever the import returned: Steam must not stay closed because of BoilR.
    let steam_start_error = if restart_steam {
        steps.start_steam().err()
    } else {
        None
    };
    ExecOutcome {
        import,
        steam_start_error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct FakeSteps {
        log: RefCell<Vec<&'static str>>,
        import: Result<(), String>,
        start: Result<(), String>,
    }

    impl FakeSteps {
        fn new(import: Result<(), String>, start: Result<(), String>) -> Self {
            Self {
                log: RefCell::new(Vec::new()),
                import,
                start,
            }
        }
    }

    impl SyncSteps for FakeSteps {
        fn stop_steam(&self) {
            self.log.borrow_mut().push("stop");
        }
        fn start_steam(&self) -> Result<(), String> {
            self.log.borrow_mut().push("start");
            self.start.clone()
        }
        fn import(&self) -> Result<(), String> {
            self.log.borrow_mut().push("import");
            self.import.clone()
        }
    }

    #[test]
    fn a_restart_stops_steam_imports_then_starts_it() {
        let steps = FakeSteps::new(Ok(()), Ok(()));
        let outcome = run_autosync(&steps, true);
        assert_eq!(*steps.log.borrow(), vec!["stop", "import", "start"]);
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
        assert_eq!(*steps.log.borrow(), vec!["stop", "import", "start"]);
        assert_eq!(outcome.import, Err("disk full".to_string()));
    }

    #[test]
    fn without_a_restart_steam_is_left_alone() {
        let steps = FakeSteps::new(Ok(()), Ok(()));
        run_autosync(&steps, false);
        assert_eq!(*steps.log.borrow(), vec!["import"]);
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
}
