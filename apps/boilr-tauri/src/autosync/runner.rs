//! The loop that owns the state machine: a full check every `interval_minutes`, a probe
//! every 5 s while something is pending, and the user's answers from the tray and the
//! prompt window. Effects become calls on an `AutosyncUi` and on `Work`.

use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use boilr_core::{autosync::AutosyncSettings, settings::Settings};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use super::{
    executor::ExecOutcome,
    machine::{plural, CheckResult, Effect, Event, Machine, Millis, State},
    probes::{GameSignal, Probes},
};

pub const PROBE_TICK: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunnerInput {
    RestartNow,
    NotNow,
    SyncNow,
    TogglePause,
    SettingsChanged,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptInfo {
    pub summary: String,
    pub deadline_ms: Millis,
}

/// What the tray tooltip, the prompt window and the settings page show.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutosyncStatus {
    pub available: bool,
    pub enabled: bool,
    pub paused: bool,
    pub summary: String,
    pub prompt: Option<PromptInfo>,
    pub steam_running: Option<bool>,
    pub game_signals: Vec<GameSignal>,
    pub last_check_ms: Option<Millis>,
    pub last_check: Option<String>,
}

pub type SharedStatus = Arc<Mutex<AutosyncStatus>>;

pub fn lock_status(status: &SharedStatus) -> MutexGuard<'_, AutosyncStatus> {
    status
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub struct CheckOutput {
    pub result: CheckResult,
    pub game_dirs: Vec<PathBuf>,
}

/// Everything that is slow or touches the disk. Called from blocking threads.
pub trait Work: Send + Sync + 'static {
    fn load_settings(&self) -> Result<Settings, String>;
    fn check(&self, settings: &Settings) -> Result<CheckOutput, String>;
    fn sync(&self, settings: &Settings, restart_steam: bool) -> ExecOutcome;
}

/// Tray, prompt window, notifications and start with Windows.
pub trait AutosyncUi: Send + Sync {
    /// Opens the prompt; it reads its text and deadline from `AutosyncStatus::prompt`.
    fn show_prompt(&self);
    fn hide_prompt(&self);
    fn notify(&self, text: &str);
    fn set_tooltip(&self, text: &str);
    fn set_paused(&self, paused: bool);
    fn set_autostart(&self, enabled: bool);
}

/// Lets commands and the tray reach the runner; managed as Tauri state on every platform.
#[derive(Clone)]
pub struct AutosyncHandle {
    pub inputs: Option<UnboundedSender<RunnerInput>>,
    pub status: SharedStatus,
    pub probes: Option<Arc<dyn Probes>>,
}

impl AutosyncHandle {
    pub fn unavailable() -> Self {
        Self {
            inputs: None,
            status: Arc::new(Mutex::new(AutosyncStatus::default())),
            probes: None,
        }
    }

    pub fn send(&self, input: RunnerInput) -> Result<(), String> {
        match &self.inputs {
            Some(inputs) => inputs
                .send(input)
                .map_err(|_| "Autosync has stopped".to_string()),
            None => Err("Autosync is only available on Windows".to_string()),
        }
    }

    /// Closing the main window hides it instead of quitting while this is true.
    pub fn keeps_running(&self) -> bool {
        let status = lock_status(&self.status);
        status.available && status.enabled
    }
}

pub fn now_ms() -> Millis {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(millis)
        .unwrap_or(0)
}

fn millis(duration: Duration) -> Millis {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// The tray tooltip and the settings page status line.
pub fn describe(state: &State, enabled: bool, paused: bool, blocked: &[String]) -> String {
    if !enabled {
        return "Autosync is off".to_string();
    }
    // A Sync now while paused still runs its countdown; show that instead.
    if paused && matches!(state, State::Idle) {
        return "Autosync paused".to_string();
    }
    if !blocked.is_empty() {
        return format!("Blocked: {}", blocked.join(", "));
    }
    match state {
        State::Idle => "Up to date".to_string(),
        State::Syncing {
            restart_steam: true,
            ..
        } => "Restarting Steam and importing games".to_string(),
        State::Syncing { .. } => "Importing games".to_string(),
        State::WaitingForGame { change }
        | State::Countdown { change, .. }
        | State::Deferred { change, .. } => format!("{} pending", plural(change.count(), "game")),
    }
}

fn describe_result(result: &CheckResult) -> String {
    match result {
        CheckResult::NoChange => "No changes".to_string(),
        CheckResult::Changed(change) => change.summary(),
        CheckResult::Blocked { platforms } => {
            format!("Blocked: {} could not be read", platforms.join(", "))
        }
    }
}

pub struct Runner {
    machine: Machine,
    settings: Option<Settings>,
    paused: bool,
    blocked: Vec<String>,
    work: Arc<dyn Work>,
    probes: Arc<dyn Probes>,
    ui: Arc<dyn AutosyncUi>,
    status: SharedStatus,
}

impl Runner {
    pub fn new(
        work: Arc<dyn Work>,
        probes: Arc<dyn Probes>,
        ui: Arc<dyn AutosyncUi>,
        status: SharedStatus,
    ) -> Self {
        Self {
            machine: Machine::new(millis(AutosyncSettings::default().countdown())),
            settings: None,
            paused: false,
            blocked: Vec::new(),
            work,
            probes,
            ui,
            status,
        }
    }

    pub fn is_idle(&self) -> bool {
        matches!(self.machine.state(), State::Idle)
    }

    pub fn interval(&self) -> Duration {
        self.settings
            .as_ref()
            .map(|settings| settings.autosync.interval())
            .unwrap_or_else(|| AutosyncSettings::default().interval())
    }

    fn enabled(&self) -> bool {
        self.settings
            .as_ref()
            .is_some_and(|settings| settings.autosync.enabled)
    }

    /// The full check. `forced` (Sync now) ignores the pause.
    pub async fn check(&mut self, now: Millis, forced: bool) {
        if !self.reload_settings(now).await || !self.enabled() || (self.paused && !forced) {
            self.publish();
            return;
        }
        let Some(settings) = self.settings.clone() else {
            return;
        };
        let work = Arc::clone(&self.work);
        let checked = tokio::task::spawn_blocking(move || work.check(&settings))
            .await
            .map_err(|err| err.to_string())
            .and_then(|result| result);
        let output = match checked {
            Ok(output) => output,
            Err(err) => {
                self.blocked.clear();
                self.record_check(now, format!("Check failed: {err}"));
                self.publish();
                return;
            }
        };
        self.probes.set_game_dirs(output.game_dirs);
        self.blocked = match &output.result {
            CheckResult::Blocked { platforms } => platforms.clone(),
            _ => Vec::new(),
        };
        self.record_check(now, describe_result(&output.result));
        let (steam_running, game_running) = self.read_probes();
        self.handle(Event::Checked {
            result: output.result,
            steam_running,
            game_running,
            now,
        })
        .await;
    }

    /// The cheap 5 s tick: only the OS readings. It runs while paused too: a pause resets
    /// to `Idle`, so only a Sync now can be pending then, and its countdown must finish.
    pub async fn probe(&mut self, now: Millis) {
        if !self.enabled() {
            return;
        }
        let (steam_running, game_running) = self.read_probes();
        self.handle(Event::Probed {
            steam_running,
            game_running,
            now,
        })
        .await;
    }

    pub async fn input(&mut self, input: RunnerInput, now: Millis) {
        match input {
            RunnerInput::RestartNow => {
                let (steam_running, game_running) = self.read_probes();
                self.handle(Event::RestartNow {
                    steam_running,
                    game_running,
                })
                .await;
            }
            RunnerInput::NotNow => self.handle(Event::NotNow).await,
            RunnerInput::SyncNow => {
                if self.is_idle() {
                    self.check(now, true).await;
                } else {
                    let (steam_running, game_running) = self.read_probes();
                    self.handle(Event::SyncNow {
                        steam_running,
                        game_running,
                    })
                    .await;
                }
            }
            RunnerInput::TogglePause => {
                self.paused = !self.paused;
                if self.paused {
                    // Nothing pending survives a pause: a countdown would restart Steam.
                    self.reset().await;
                }
                self.ui.set_paused(self.paused);
                self.publish();
            }
            RunnerInput::SettingsChanged => {
                self.reload_settings(now).await;
                self.publish();
            }
        }
    }

    /// Whether settings could be read. When not, the check is skipped and recorded.
    async fn reload_settings(&mut self, now: Millis) -> bool {
        let work = Arc::clone(&self.work);
        let loaded = tokio::task::spawn_blocking(move || work.load_settings())
            .await
            .map_err(|err| err.to_string())
            .and_then(|result| result);
        let settings = match loaded {
            Ok(settings) => settings,
            Err(err) => {
                eprintln!("Autosync skipped a check, the settings could not be read: {err}");
                self.blocked.clear();
                self.record_check(now, format!("Settings unreadable: {err}"));
                return false;
            }
        };
        let previous = self
            .settings
            .as_ref()
            .map(|settings| settings.autosync.clone());
        if previous.as_ref() != Some(&settings.autosync) {
            self.ui
                .set_autostart(settings.autosync.enabled && settings.autosync.start_with_windows);
        }
        self.machine
            .set_countdown(millis(settings.autosync.countdown()));
        let enabled = settings.autosync.enabled;
        self.settings = Some(settings);
        if !enabled {
            self.reset().await;
        }
        true
    }

    /// Back to `Idle` (a running import finishes first).
    async fn reset(&mut self) {
        for effect in self.machine.reset() {
            self.apply(effect).await;
        }
    }

    fn read_probes(&self) -> (bool, bool) {
        let steam_running = self.probes.steam_running();
        let signals = self.probes.game_signals();
        let game_running = !signals.is_empty();
        let mut status = lock_status(&self.status);
        status.steam_running = Some(steam_running);
        status.game_signals = signals;
        (steam_running, game_running)
    }

    async fn handle(&mut self, event: Event) {
        let mut queue = VecDeque::from([event]);
        while let Some(event) = queue.pop_front() {
            for effect in self.machine.handle(event) {
                if let Some(next) = self.apply(effect).await {
                    queue.push_back(next);
                }
            }
        }
        self.publish();
    }

    async fn apply(&mut self, effect: Effect) -> Option<Event> {
        match effect {
            Effect::ShowPrompt { change, deadline } => {
                lock_status(&self.status).prompt = Some(PromptInfo {
                    summary: change.summary(),
                    deadline_ms: deadline,
                });
                self.ui.show_prompt();
                None
            }
            Effect::HidePrompt => {
                lock_status(&self.status).prompt = None;
                self.ui.hide_prompt();
                None
            }
            Effect::Notify(text) => {
                self.ui.notify(&text);
                None
            }
            Effect::StartSync { restart_steam } => {
                self.publish();
                let outcome = self.run_sync(restart_steam).await;
                if let Some(err) = &outcome.steam_start_error {
                    self.ui
                        .notify(&format!("Steam could not be started again: {err}"));
                }
                Some(Event::SyncFinished {
                    error: outcome.import.err(),
                })
            }
        }
    }

    async fn run_sync(&self, restart_steam: bool) -> ExecOutcome {
        let Some(settings) = self.settings.clone() else {
            return ExecOutcome {
                import: Err("Settings are not loaded".to_string()),
                steam_start_error: None,
            };
        };
        let work = Arc::clone(&self.work);
        tokio::task::spawn_blocking(move || work.sync(&settings, restart_steam))
            .await
            .unwrap_or_else(|err| ExecOutcome {
                import: Err(format!("The import stopped unexpectedly: {err}")),
                steam_start_error: None,
            })
    }

    fn record_check(&self, now: Millis, text: String) {
        let mut status = lock_status(&self.status);
        status.last_check_ms = Some(now);
        status.last_check = Some(text);
    }

    fn publish(&self) {
        let summary = describe(
            self.machine.state(),
            self.enabled(),
            self.paused,
            &self.blocked,
        );
        {
            let mut status = lock_status(&self.status);
            status.enabled = self.enabled();
            status.paused = self.paused;
            status.summary = summary.clone();
        }
        self.ui.set_tooltip(&summary);
    }
}

/// Runs until every input sender is gone. The first check happens at once.
pub async fn run_loop(mut runner: Runner, mut inputs: UnboundedReceiver<RunnerInput>) {
    let mut ticker = tokio::time::interval(PROBE_TICK);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut next_check = tokio::time::Instant::now();
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                if tokio::time::Instant::now() >= next_check {
                    runner.check(now_ms(), false).await;
                    next_check = tokio::time::Instant::now() + runner.interval();
                } else if !runner.is_idle() {
                    runner.probe(now_ms()).await;
                }
            }
            input = inputs.recv() => match input {
                Some(input) => runner.input(input, now_ms()).await,
                None => break,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::autosync::machine::ChangeSet;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    struct FakeWork {
        settings: Result<Settings, String>,
        results: Mutex<VecDeque<CheckResult>>,
        checks: AtomicUsize,
        syncs: Mutex<Vec<bool>>,
        outcome: ExecOutcome,
    }

    impl Work for FakeWork {
        fn load_settings(&self) -> Result<Settings, String> {
            self.settings.clone()
        }
        fn check(&self, _settings: &Settings) -> Result<CheckOutput, String> {
            self.checks.fetch_add(1, Ordering::SeqCst);
            let result = self
                .results
                .lock()
                .expect("results lock")
                .pop_front()
                .unwrap_or(CheckResult::NoChange);
            Ok(CheckOutput {
                result,
                game_dirs: Vec::new(),
            })
        }
        fn sync(&self, _settings: &Settings, restart_steam: bool) -> ExecOutcome {
            self.syncs.lock().expect("syncs lock").push(restart_steam);
            self.outcome.clone()
        }
    }

    struct FakeProbes {
        steam: AtomicBool,
        game: AtomicBool,
    }

    impl Probes for FakeProbes {
        fn steam_running(&self) -> bool {
            self.steam.load(Ordering::SeqCst)
        }
        fn game_signals(&self) -> Vec<GameSignal> {
            if self.game.load(Ordering::SeqCst) {
                vec![GameSignal::FullScreen]
            } else {
                Vec::new()
            }
        }
        fn set_game_dirs(&self, _dirs: Vec<PathBuf>) {}
    }

    #[derive(Default)]
    struct FakeUi {
        calls: Mutex<Vec<String>>,
    }

    impl FakeUi {
        fn record(&self, call: String) {
            self.calls.lock().expect("ui lock").push(call);
        }
        fn calls(&self) -> Vec<String> {
            self.calls.lock().expect("ui lock").clone()
        }
    }

    impl AutosyncUi for FakeUi {
        fn show_prompt(&self) {
            self.record("show_prompt".to_string());
        }
        fn hide_prompt(&self) {
            self.record("hide_prompt".to_string());
        }
        fn notify(&self, text: &str) {
            self.record(format!("notify:{text}"));
        }
        fn set_tooltip(&self, text: &str) {
            self.record(format!("tooltip:{text}"));
        }
        fn set_paused(&self, paused: bool) {
            self.record(format!("paused:{paused}"));
        }
        fn set_autostart(&self, enabled: bool) {
            self.record(format!("autostart:{enabled}"));
        }
    }

    /// Built from the embedded defaults, never from the user's config file.
    fn settings(enabled: bool) -> Settings {
        let mut settings: Settings = toml::from_str(include_str!(
            "../../../../crates/boilr-core/src/defaultconfig.toml"
        ))
        .expect("default config fills Settings");
        settings.autosync.enabled = enabled;
        settings
    }

    fn ok() -> ExecOutcome {
        ExecOutcome {
            import: Ok(()),
            steam_start_error: None,
        }
    }

    fn one_new_game() -> CheckResult {
        CheckResult::Changed(ChangeSet {
            added: [1].into_iter().collect(),
            removed: Default::default(),
        })
    }

    fn work(
        settings: Result<Settings, String>,
        results: Vec<CheckResult>,
        outcome: ExecOutcome,
    ) -> Arc<FakeWork> {
        Arc::new(FakeWork {
            settings,
            results: Mutex::new(results.into()),
            checks: AtomicUsize::new(0),
            syncs: Mutex::new(Vec::new()),
            outcome,
        })
    }

    struct Harness {
        runner: Runner,
        ui: Arc<FakeUi>,
        probes: Arc<FakeProbes>,
        status: SharedStatus,
    }

    fn harness(work: Arc<FakeWork>, steam: bool, game: bool) -> Harness {
        let probes = Arc::new(FakeProbes {
            steam: AtomicBool::new(steam),
            game: AtomicBool::new(game),
        });
        let ui = Arc::new(FakeUi::default());
        let status: SharedStatus = Arc::new(Mutex::new(AutosyncStatus {
            available: true,
            ..Default::default()
        }));
        let runner = Runner::new(work, probes.clone(), ui.clone(), status.clone());
        Harness {
            runner,
            ui,
            probes,
            status,
        }
    }

    #[tokio::test]
    async fn a_change_with_steam_closed_imports_without_restarting_steam() {
        let work = work(Ok(settings(true)), vec![one_new_game()], ok());
        let mut h = harness(work.clone(), false, false);
        h.runner.check(1_000, false).await;
        assert_eq!(*work.syncs.lock().expect("lock"), vec![false]);
        assert!(h.runner.is_idle());
        assert_eq!(lock_status(&h.status).summary, "Up to date");
        assert!(h.ui.calls().contains(&"tooltip:Up to date".to_string()));
    }

    #[tokio::test]
    async fn a_paused_runner_skips_checks_but_sync_now_still_checks() {
        let work = work(Ok(settings(true)), vec![], ok());
        let mut h = harness(work.clone(), true, false);
        h.runner.input(RunnerInput::TogglePause, 0).await;
        h.runner.check(1_000, false).await;
        assert_eq!(work.checks.load(Ordering::SeqCst), 0);
        assert!(h.ui.calls().contains(&"paused:true".to_string()));
        assert_eq!(lock_status(&h.status).summary, "Autosync paused");

        h.runner.input(RunnerInput::SyncNow, 2_000).await;
        assert_eq!(work.checks.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn pausing_during_the_countdown_hides_the_prompt_and_never_syncs() {
        let work = work(
            Ok(settings(true)),
            vec![one_new_game(), one_new_game()],
            ok(),
        );
        let mut h = harness(work.clone(), true, false);
        h.runner.check(1_000, false).await;
        assert!(lock_status(&h.status).prompt.is_some());

        h.runner.input(RunnerInput::TogglePause, 2_000).await;
        assert!(h.runner.is_idle());
        assert!(lock_status(&h.status).prompt.is_none());
        assert_eq!(
            h.ui.calls().last(),
            Some(&"tooltip:Autosync paused".to_string())
        );
        assert!(h.ui.calls().contains(&"hide_prompt".to_string()));

        // Well past the countdown: neither the probe tick nor the full check may import.
        let later = 10 * 60 * 1_000;
        h.runner.probe(later).await;
        h.runner.check(later + 1, false).await;
        assert!(work.syncs.lock().expect("lock").is_empty());
        assert_eq!(work.checks.load(Ordering::SeqCst), 1);
        assert!(h.runner.is_idle());
    }

    #[tokio::test]
    async fn sync_now_while_paused_finishes_its_countdown() {
        let work = work(Ok(settings(true)), vec![one_new_game()], ok());
        let mut h = harness(work.clone(), true, false);
        h.runner.input(RunnerInput::TogglePause, 0).await;
        h.runner.input(RunnerInput::SyncNow, 1_000).await;
        assert!(lock_status(&h.status).prompt.is_some());
        assert_eq!(lock_status(&h.status).summary, "1 game pending");

        h.runner.probe(10 * 60 * 1_000).await;
        assert_eq!(*work.syncs.lock().expect("lock"), vec![true]);
        assert!(h.runner.is_idle());
        assert_eq!(lock_status(&h.status).summary, "Autosync paused");
    }

    #[tokio::test]
    async fn a_failed_check_forgets_the_blocked_platforms() {
        struct Flaky {
            inner: Arc<FakeWork>,
            fail: AtomicBool,
        }
        impl Work for Flaky {
            fn load_settings(&self) -> Result<Settings, String> {
                self.inner.load_settings()
            }
            fn check(&self, settings: &Settings) -> Result<CheckOutput, String> {
                if self.fail.load(Ordering::SeqCst) {
                    Err("shortcuts.vdf unreadable".to_string())
                } else {
                    self.inner.check(settings)
                }
            }
            fn sync(&self, settings: &Settings, restart_steam: bool) -> ExecOutcome {
                self.inner.sync(settings, restart_steam)
            }
        }
        let blocked = CheckResult::Blocked {
            platforms: vec!["Epic".to_string()],
        };
        let flaky = Arc::new(Flaky {
            inner: work(Ok(settings(true)), vec![blocked], ok()),
            fail: AtomicBool::new(false),
        });
        let probes = Arc::new(FakeProbes {
            steam: AtomicBool::new(false),
            game: AtomicBool::new(false),
        });
        let status: SharedStatus = Arc::new(Mutex::new(AutosyncStatus::default()));
        let mut runner = Runner::new(
            flaky.clone(),
            probes,
            Arc::new(FakeUi::default()),
            status.clone(),
        );
        runner.check(1_000, false).await;
        assert_eq!(lock_status(&status).summary, "Blocked: Epic");

        flaky.fail.store(true, Ordering::SeqCst);
        runner.check(2_000, false).await;
        assert_eq!(lock_status(&status).summary, "Up to date");
        assert_eq!(
            lock_status(&status).last_check.as_deref(),
            Some("Check failed: shortcuts.vdf unreadable")
        );
    }

    #[tokio::test]
    async fn a_blocked_check_does_not_sync_and_names_the_platform() {
        let blocked = CheckResult::Blocked {
            platforms: vec!["Epic".to_string()],
        };
        let work = work(Ok(settings(true)), vec![blocked], ok());
        let mut h = harness(work.clone(), false, false);
        h.runner.check(1_000, false).await;
        assert!(work.syncs.lock().expect("lock").is_empty());
        assert_eq!(lock_status(&h.status).summary, "Blocked: Epic");
        assert!(h.ui.calls().contains(&"tooltip:Blocked: Epic".to_string()));
    }

    #[tokio::test]
    async fn unreadable_settings_skip_the_check_and_say_so() {
        let work = work(
            Err("expected an integer".to_string()),
            vec![one_new_game()],
            ok(),
        );
        let mut h = harness(work.clone(), false, false);
        h.runner.check(1_000, false).await;
        assert_eq!(work.checks.load(Ordering::SeqCst), 0);
        let status = lock_status(&h.status);
        assert_eq!(status.last_check_ms, Some(1_000));
        assert_eq!(
            status.last_check.as_deref(),
            Some("Settings unreadable: expected an integer")
        );
    }

    #[tokio::test]
    async fn disabled_autosync_does_not_check_and_leaves_windows_startup() {
        let work = work(Ok(settings(false)), vec![one_new_game()], ok());
        let mut h = harness(work.clone(), false, false);
        h.runner.check(1_000, false).await;
        assert_eq!(work.checks.load(Ordering::SeqCst), 0);
        assert!(h.ui.calls().contains(&"autostart:false".to_string()));
        assert_eq!(lock_status(&h.status).summary, "Autosync is off");
    }

    #[tokio::test]
    async fn a_failed_import_after_restart_reports_both_errors() {
        let outcome = ExecOutcome {
            import: Err("disk full".to_string()),
            steam_start_error: Some("Steam was not found".to_string()),
        };
        let work = work(Ok(settings(true)), vec![one_new_game()], outcome);
        let mut h = harness(work.clone(), true, false);

        h.runner.check(1_000, false).await;
        assert!(lock_status(&h.status).prompt.is_some());
        assert!(h.ui.calls().contains(&"show_prompt".to_string()));

        h.runner.input(RunnerInput::RestartNow, 2_000).await;
        assert_eq!(*work.syncs.lock().expect("lock"), vec![true]);
        let calls = h.ui.calls();
        assert!(calls
            .contains(&"notify:Steam could not be started again: Steam was not found".to_string()));
        assert!(calls
            .iter()
            .any(|call| call.starts_with("notify:Import failed: disk full")));
        assert!(lock_status(&h.status).prompt.is_none());
        assert_eq!(lock_status(&h.status).summary, "1 game pending");
    }

    #[tokio::test]
    async fn restart_now_reads_the_probes_and_never_restarts_steam_under_a_game() {
        let work = work(Ok(settings(true)), vec![one_new_game()], ok());
        let mut h = harness(work.clone(), true, false);
        h.runner.check(1_000, false).await;
        assert!(lock_status(&h.status).prompt.is_some());

        h.probes.game.store(true, Ordering::SeqCst);
        h.runner.input(RunnerInput::RestartNow, 2_000).await;
        assert!(work.syncs.lock().expect("lock").is_empty());
        assert_eq!(
            lock_status(&h.status).game_signals,
            vec![GameSignal::FullScreen]
        );
        assert!(!h.runner.is_idle());
    }

    #[test]
    fn tooltip_texts() {
        let pending = State::Deferred {
            change: ChangeSet {
                added: [1, 2].into_iter().collect(),
                removed: Default::default(),
            },
            after_failure: false,
        };
        assert_eq!(describe(&State::Idle, false, false, &[]), "Autosync is off");
        assert_eq!(describe(&State::Idle, true, true, &[]), "Autosync paused");
        assert_eq!(
            describe(&State::Idle, true, false, &["Epic".to_string()]),
            "Blocked: Epic"
        );
        assert_eq!(describe(&State::Idle, true, false, &[]), "Up to date");
        assert_eq!(describe(&pending, true, false, &[]), "2 games pending");
        assert_eq!(describe(&pending, true, true, &[]), "2 games pending");
    }

    #[test]
    fn an_unavailable_handle_refuses_input_and_never_keeps_the_app_running() {
        let handle = AutosyncHandle::unavailable();
        assert!(handle.send(RunnerInput::SyncNow).is_err());
        assert!(!handle.keeps_running());
    }
}
