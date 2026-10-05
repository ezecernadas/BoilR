//! The autosync behaviour table as a pure state machine. No Tauri, no OS calls and no
//! clock: time comes in with the events, as milliseconds since the Unix epoch.

use std::collections::BTreeSet;

pub type Millis = u64;

pub const WAITS_FOR_GAME: &str = "A game is running. The import waits until you stop playing.";
pub const UNCHANGED_BY_IMPORT: &str =
    "The last import did not change Steam. Pick Sync now in the tray to try again.";

pub fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

/// What a sync would change in Steam: app ids it would add and remove. Its identity tells
/// a deferred change from a new one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChangeSet {
    pub added: BTreeSet<u32>,
    pub removed: BTreeSet<u32>,
}

impl ChangeSet {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty()
    }

    pub fn count(&self) -> usize {
        self.added.len() + self.removed.len()
    }

    pub fn summary(&self) -> String {
        match (self.added.len(), self.removed.len()) {
            (added, 0) => plural(added, "new game"),
            (0, removed) => format!("{} removed", plural(removed, "game")),
            (added, removed) => format!("{}, {removed} removed", plural(added, "new game")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckResult {
    NoChange,
    Changed(ChangeSet),
    /// An enabled launcher failed: syncing now would delete its games from Steam.
    Blocked {
        platforms: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    Idle,
    WaitingForGame {
        change: ChangeSet,
    },
    Countdown {
        change: ChangeSet,
        deadline: Millis,
    },
    /// `after_failure`: the import failed, so finding Steam closed does not retry it;
    /// only Sync now or a different change set does.
    Deferred {
        change: ChangeSet,
        after_failure: bool,
    },
    Syncing {
        change: ChangeSet,
        restart_steam: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Checked {
        result: CheckResult,
        steam_running: bool,
        game_running: bool,
        now: Millis,
    },
    Probed {
        steam_running: bool,
        game_running: bool,
        now: Millis,
    },
    RestartNow {
        steam_running: bool,
        game_running: bool,
    },
    NotNow,
    SyncNow {
        steam_running: bool,
        game_running: bool,
    },
    SyncFinished {
        error: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    ShowPrompt { change: ChangeSet, deadline: Millis },
    HidePrompt,
    Notify(String),
    StartSync { restart_steam: bool },
}

pub struct Machine {
    state: State,
    countdown_ms: Millis,
    /// The change set of the last import that succeeded, until a check finds no change
    /// or a different one. Seeing it again means the import left Steam as it was.
    last_synced: Option<ChangeSet>,
}

impl Machine {
    pub fn new(countdown_ms: Millis) -> Self {
        Self {
            state: State::Idle,
            countdown_ms,
            last_synced: None,
        }
    }

    pub fn state(&self) -> &State {
        &self.state
    }

    pub fn set_countdown(&mut self, countdown_ms: Millis) {
        self.countdown_ms = countdown_ms;
    }

    pub fn handle(&mut self, event: Event) -> Vec<Effect> {
        let state = std::mem::replace(&mut self.state, State::Idle);
        let (next, effects) = self.transition(state, event);
        self.state = next;
        effects
    }

    /// Back to `Idle`, for when autosync is switched off. A running import finishes first.
    pub fn reset(&mut self) -> Vec<Effect> {
        match self.state {
            State::Syncing { .. } => Vec::new(),
            State::Countdown { .. } => {
                self.state = State::Idle;
                vec![Effect::HidePrompt]
            }
            _ => {
                self.state = State::Idle;
                Vec::new()
            }
        }
    }

    fn transition(&mut self, state: State, event: Event) -> (State, Vec<Effect>) {
        match (state, event) {
            (
                state,
                Event::Checked {
                    result: CheckResult::Blocked { .. },
                    ..
                },
            ) => (state, Vec::new()),
            (State::Syncing { change, .. }, Event::SyncFinished { error: None }) => {
                self.last_synced = Some(change);
                (State::Idle, Vec::new())
            }
            (State::Syncing { change, .. }, Event::SyncFinished { error: Some(error) }) => (
                State::Deferred {
                    change,
                    after_failure: true,
                },
                vec![Effect::Notify(format!(
                    "Import failed: {error}. Pick Sync now in the tray to try again."
                ))],
            ),
            (state @ State::Syncing { .. }, _) => (state, Vec::new()),
            (
                State::Idle,
                Event::Checked {
                    result: CheckResult::Changed(change),
                    steam_running,
                    game_running,
                    now,
                },
            ) => {
                if self.last_synced.as_ref() == Some(&change) {
                    // Importing it again would restart Steam at every check.
                    (
                        State::Deferred {
                            change,
                            after_failure: true,
                        },
                        vec![Effect::Notify(UNCHANGED_BY_IMPORT.to_string())],
                    )
                } else {
                    self.last_synced = None;
                    self.start(change, steam_running, game_running, now)
                }
            }
            (
                State::Idle,
                Event::Checked {
                    result: CheckResult::NoChange,
                    ..
                },
            ) => {
                self.last_synced = None;
                (State::Idle, Vec::new())
            }
            (State::Idle, _) => (State::Idle, Vec::new()),
            (
                _,
                Event::Checked {
                    result: CheckResult::NoChange,
                    ..
                },
            ) => {
                self.last_synced = None;
                (State::Idle, vec![Effect::HidePrompt])
            }
            (
                State::Deferred {
                    change: deferred,
                    after_failure,
                },
                Event::Checked {
                    result: CheckResult::Changed(change),
                    steam_running,
                    game_running,
                    now,
                },
            ) => {
                if change == deferred {
                    self.probe(
                        State::Deferred {
                            change,
                            after_failure,
                        },
                        steam_running,
                        game_running,
                        now,
                    )
                } else {
                    self.start(change, steam_running, game_running, now)
                }
            }
            (
                State::WaitingForGame { .. },
                Event::Checked {
                    result: CheckResult::Changed(change),
                    steam_running,
                    game_running,
                    now,
                },
            ) => self.probe(
                State::WaitingForGame { change },
                steam_running,
                game_running,
                now,
            ),
            (
                State::Countdown {
                    change: shown,
                    deadline,
                },
                Event::Checked {
                    result: CheckResult::Changed(change),
                    steam_running,
                    game_running,
                    now,
                },
            ) => {
                let mut effects = Vec::new();
                if change != shown {
                    effects.push(Effect::ShowPrompt {
                        change: change.clone(),
                        deadline,
                    });
                }
                let (next, more) = self.probe(
                    State::Countdown { change, deadline },
                    steam_running,
                    game_running,
                    now,
                );
                effects.extend(more);
                (next, effects)
            }
            (
                state,
                Event::Probed {
                    steam_running,
                    game_running,
                    now,
                },
            ) => self.probe(state, steam_running, game_running, now),
            (
                State::Countdown { change, .. },
                Event::RestartNow {
                    steam_running,
                    game_running,
                }
                | Event::SyncNow {
                    steam_running,
                    game_running,
                },
            ) => {
                let waiting = State::WaitingForGame {
                    change: change.clone(),
                };
                let (next, mut effects) =
                    Self::manual_sync(change, waiting, steam_running, game_running);
                effects.insert(0, Effect::HidePrompt);
                (next, effects)
            }
            (State::Countdown { change, .. }, Event::NotNow) => (
                State::Deferred {
                    change,
                    after_failure: false,
                },
                vec![Effect::HidePrompt],
            ),
            (
                state @ (State::Deferred { .. } | State::WaitingForGame { .. }),
                Event::SyncNow {
                    steam_running,
                    game_running,
                },
            ) => Self::sync_now(state, steam_running, game_running),
            (state, _) => (state, Vec::new()),
        }
    }

    /// What the latest readings mean for a waiting state.
    fn probe(
        &self,
        state: State,
        steam_running: bool,
        game_running: bool,
        now: Millis,
    ) -> (State, Vec<Effect>) {
        match state {
            // Steam closed: importing touches no game, whatever the game signals say (a
            // stale Steam "running app" would otherwise keep this state forever).
            State::WaitingForGame { change } if !steam_running => Self::sync(change, false),
            State::WaitingForGame { change } if !game_running => self.start_countdown(change, now),
            State::Countdown { change, deadline } if now >= deadline => {
                let (next, mut effects) = if !steam_running {
                    // The user closed Steam meanwhile: import, but don't start it again.
                    Self::sync(change, false)
                } else if game_running {
                    (State::WaitingForGame { change }, Vec::new())
                } else {
                    Self::sync(change, true)
                };
                effects.insert(0, Effect::HidePrompt);
                (next, effects)
            }
            State::Deferred {
                change,
                after_failure: false,
            } if !steam_running => Self::sync(change, false),
            other => (other, Vec::new()),
        }
    }

    fn start(
        &self,
        change: ChangeSet,
        steam_running: bool,
        game_running: bool,
        now: Millis,
    ) -> (State, Vec<Effect>) {
        if !steam_running {
            Self::sync(change, false)
        } else if !game_running {
            self.start_countdown(change, now)
        } else {
            let text = format!(
                "{}, they will be imported when you stop playing",
                change.summary()
            );
            (State::WaitingForGame { change }, vec![Effect::Notify(text)])
        }
    }

    fn start_countdown(&self, change: ChangeSet, now: Millis) -> (State, Vec<Effect>) {
        let deadline = now.saturating_add(self.countdown_ms);
        (
            State::Countdown {
                change: change.clone(),
                deadline,
            },
            vec![Effect::ShowPrompt { change, deadline }],
        )
    }

    fn sync(change: ChangeSet, restart_steam: bool) -> (State, Vec<Effect>) {
        (
            State::Syncing {
                change,
                restart_steam,
            },
            vec![Effect::StartSync { restart_steam }],
        )
    }

    fn sync_now(state: State, steam_running: bool, game_running: bool) -> (State, Vec<Effect>) {
        let change = match &state {
            State::Deferred { change, .. } | State::WaitingForGame { change } => change.clone(),
            _ => return (state, Vec::new()),
        };
        Self::manual_sync(change, state, steam_running, game_running)
    }

    /// A user-requested import (Sync now, Restart now). Steam closed: import without
    /// restarting it. Steam open under a running game: never restart it, stay in `waiting`.
    fn manual_sync(
        change: ChangeSet,
        waiting: State,
        steam_running: bool,
        game_running: bool,
    ) -> (State, Vec<Effect>) {
        if !steam_running {
            Self::sync(change, false)
        } else if game_running {
            (waiting, vec![Effect::Notify(WAITS_FOR_GAME.to_string())])
        } else {
            Self::sync(change, true)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const COUNTDOWN: Millis = 60_000;

    fn change(added: &[u32], removed: &[u32]) -> ChangeSet {
        ChangeSet {
            added: added.iter().copied().collect(),
            removed: removed.iter().copied().collect(),
        }
    }

    fn machine_in(state: State) -> Machine {
        let mut machine = Machine::new(COUNTDOWN);
        machine.state = state;
        machine
    }

    fn changed(change: &ChangeSet, steam_running: bool, game_running: bool, now: Millis) -> Event {
        Event::Checked {
            result: CheckResult::Changed(change.clone()),
            steam_running,
            game_running,
            now,
        }
    }

    fn no_change(now: Millis) -> Event {
        Event::Checked {
            result: CheckResult::NoChange,
            steam_running: true,
            game_running: false,
            now,
        }
    }

    fn probed(steam_running: bool, game_running: bool, now: Millis) -> Event {
        Event::Probed {
            steam_running,
            game_running,
            now,
        }
    }

    fn playing_text(change: &ChangeSet) -> String {
        format!(
            "{}, they will be imported when you stop playing",
            change.summary()
        )
    }

    #[test]
    fn summary_names_added_and_removed_games() {
        assert_eq!(change(&[1], &[]).summary(), "1 new game");
        assert_eq!(change(&[1, 2], &[]).summary(), "2 new games");
        assert_eq!(change(&[], &[3]).summary(), "1 game removed");
        assert_eq!(change(&[1, 2], &[3]).summary(), "2 new games, 1 removed");
    }

    #[test]
    fn idle_change_with_steam_closed_syncs_without_restart() {
        let c = change(&[1], &[]);
        let mut machine = Machine::new(COUNTDOWN);
        let effects = machine.handle(changed(&c, false, false, 0));
        assert_eq!(
            effects,
            vec![Effect::StartSync {
                restart_steam: false
            }]
        );
        assert_eq!(
            machine.state(),
            &State::Syncing {
                change: c,
                restart_steam: false
            }
        );
    }

    #[test]
    fn idle_change_with_steam_open_and_no_game_starts_the_countdown() {
        let c = change(&[1], &[]);
        let mut machine = Machine::new(COUNTDOWN);
        let effects = machine.handle(changed(&c, true, false, 1_000));
        assert_eq!(
            effects,
            vec![Effect::ShowPrompt {
                change: c.clone(),
                deadline: 61_000
            }]
        );
        assert_eq!(
            machine.state(),
            &State::Countdown {
                change: c,
                deadline: 61_000
            }
        );
    }

    #[test]
    fn idle_change_while_playing_waits_and_notifies_once() {
        let c = change(&[1, 2], &[]);
        let mut machine = Machine::new(COUNTDOWN);
        let effects = machine.handle(changed(&c, true, true, 0));
        assert_eq!(effects, vec![Effect::Notify(playing_text(&c))]);
        assert_eq!(
            machine.state(),
            &State::WaitingForGame { change: c.clone() }
        );

        let again = machine.handle(changed(&c, true, true, 300_000));
        assert!(again.is_empty(), "the same change must not notify twice");
    }

    #[test]
    fn waiting_game_stops_with_steam_open_starts_the_countdown() {
        let c = change(&[1], &[]);
        let mut machine = machine_in(State::WaitingForGame { change: c.clone() });
        let effects = machine.handle(probed(true, false, 5_000));
        assert_eq!(
            effects,
            vec![Effect::ShowPrompt {
                change: c.clone(),
                deadline: 65_000
            }]
        );
        assert_eq!(
            machine.state(),
            &State::Countdown {
                change: c,
                deadline: 65_000
            }
        );
    }

    #[test]
    fn waiting_game_stops_with_steam_closed_syncs_without_restart() {
        let c = change(&[1], &[]);
        let mut machine = machine_in(State::WaitingForGame { change: c.clone() });
        let effects = machine.handle(probed(false, false, 5_000));
        assert_eq!(
            effects,
            vec![Effect::StartSync {
                restart_steam: false
            }]
        );
        assert_eq!(
            machine.state(),
            &State::Syncing {
                change: c,
                restart_steam: false
            }
        );
    }

    #[test]
    fn waiting_while_still_playing_stays() {
        let c = change(&[1], &[]);
        let mut machine = machine_in(State::WaitingForGame { change: c.clone() });
        assert!(machine.handle(probed(true, true, 5_000)).is_empty());
        assert_eq!(machine.state(), &State::WaitingForGame { change: c });
    }

    #[test]
    fn countdown_before_the_deadline_keeps_counting() {
        let c = change(&[1], &[]);
        let state = State::Countdown {
            change: c,
            deadline: 60_000,
        };
        let mut machine = machine_in(state.clone());
        assert!(machine.handle(probed(true, false, 59_999)).is_empty());
        assert_eq!(machine.state(), &state);
    }

    #[test]
    fn countdown_deadline_passed_syncs_with_restart() {
        let c = change(&[1], &[]);
        let mut machine = machine_in(State::Countdown {
            change: c.clone(),
            deadline: 60_000,
        });
        let effects = machine.handle(probed(true, false, 60_000));
        assert_eq!(
            effects,
            vec![
                Effect::HidePrompt,
                Effect::StartSync {
                    restart_steam: true
                }
            ]
        );
        assert_eq!(
            machine.state(),
            &State::Syncing {
                change: c,
                restart_steam: true
            }
        );
    }

    #[test]
    fn countdown_deadline_with_a_game_started_waits_without_a_second_notification() {
        let c = change(&[1], &[]);
        let mut machine = machine_in(State::Countdown {
            change: c.clone(),
            deadline: 60_000,
        });
        let effects = machine.handle(probed(true, true, 61_000));
        assert_eq!(effects, vec![Effect::HidePrompt]);
        assert_eq!(machine.state(), &State::WaitingForGame { change: c });
    }

    #[test]
    fn countdown_deadline_with_steam_closed_imports_without_restart() {
        let c = change(&[1], &[]);
        let mut machine = machine_in(State::Countdown {
            change: c.clone(),
            deadline: 60_000,
        });
        let effects = machine.handle(probed(false, false, 60_000));
        assert_eq!(
            effects,
            vec![
                Effect::HidePrompt,
                Effect::StartSync {
                    restart_steam: false
                }
            ]
        );
        assert_eq!(
            machine.state(),
            &State::Syncing {
                change: c,
                restart_steam: false
            }
        );
    }

    #[test]
    fn countdown_restart_now_syncs_with_restart() {
        let c = change(&[1], &[]);
        let mut machine = machine_in(State::Countdown {
            change: c.clone(),
            deadline: 60_000,
        });
        let effects = machine.handle(Event::RestartNow {
            steam_running: true,
            game_running: false,
        });
        assert_eq!(
            effects,
            vec![
                Effect::HidePrompt,
                Effect::StartSync {
                    restart_steam: true
                }
            ]
        );
        assert_eq!(
            machine.state(),
            &State::Syncing {
                change: c,
                restart_steam: true
            }
        );
    }

    #[test]
    fn countdown_sync_now_with_steam_open_and_no_game_restarts_steam() {
        let c = change(&[1], &[]);
        let mut machine = machine_in(State::Countdown {
            change: c.clone(),
            deadline: 60_000,
        });
        let effects = machine.handle(Event::SyncNow {
            steam_running: true,
            game_running: false,
        });
        assert_eq!(
            effects,
            vec![
                Effect::HidePrompt,
                Effect::StartSync {
                    restart_steam: true
                }
            ]
        );
        assert_eq!(
            machine.state(),
            &State::Syncing {
                change: c,
                restart_steam: true
            }
        );
    }

    #[test]
    fn countdown_sync_now_with_steam_closed_imports_even_while_a_game_runs() {
        let c = change(&[1], &[]);
        let mut machine = machine_in(State::Countdown {
            change: c.clone(),
            deadline: 60_000,
        });
        let effects = machine.handle(Event::SyncNow {
            steam_running: false,
            game_running: true,
        });
        assert_eq!(
            effects,
            vec![
                Effect::HidePrompt,
                Effect::StartSync {
                    restart_steam: false
                }
            ]
        );
        assert_eq!(
            machine.state(),
            &State::Syncing {
                change: c,
                restart_steam: false
            }
        );
    }

    #[test]
    fn restart_now_during_countdown_while_playing_does_not_restart() {
        let c = change(&[1], &[]);
        let mut machine = machine_in(State::Countdown {
            change: c.clone(),
            deadline: 60_000,
        });
        let effects = machine.handle(Event::RestartNow {
            steam_running: true,
            game_running: true,
        });
        assert_eq!(
            effects,
            vec![
                Effect::HidePrompt,
                Effect::Notify(WAITS_FOR_GAME.to_string())
            ]
        );
        assert_eq!(machine.state(), &State::WaitingForGame { change: c });
    }

    #[test]
    fn sync_now_during_countdown_while_playing_does_not_restart() {
        let c = change(&[1], &[]);
        let mut machine = machine_in(State::Countdown {
            change: c.clone(),
            deadline: 60_000,
        });
        let effects = machine.handle(Event::SyncNow {
            steam_running: true,
            game_running: true,
        });
        assert_eq!(
            effects,
            vec![
                Effect::HidePrompt,
                Effect::Notify(WAITS_FOR_GAME.to_string())
            ]
        );
        assert_eq!(machine.state(), &State::WaitingForGame { change: c });
    }

    #[test]
    fn countdown_not_now_defers_and_hides_the_prompt() {
        let c = change(&[1], &[]);
        let mut machine = machine_in(State::Countdown {
            change: c.clone(),
            deadline: 60_000,
        });
        let effects = machine.handle(Event::NotNow);
        assert_eq!(effects, vec![Effect::HidePrompt]);
        assert_eq!(
            machine.state(),
            &State::Deferred {
                change: c,
                after_failure: false
            }
        );
    }

    #[test]
    fn countdown_with_a_different_change_updates_the_prompt_and_keeps_the_deadline() {
        let first = change(&[1], &[]);
        let second = change(&[1, 2], &[]);
        let mut machine = machine_in(State::Countdown {
            change: first,
            deadline: 60_000,
        });
        let effects = machine.handle(changed(&second, true, false, 30_000));
        assert_eq!(
            effects,
            vec![Effect::ShowPrompt {
                change: second.clone(),
                deadline: 60_000
            }]
        );
        assert_eq!(
            machine.state(),
            &State::Countdown {
                change: second,
                deadline: 60_000
            }
        );
    }

    #[test]
    fn deferred_with_steam_closed_syncs_without_restart() {
        let c = change(&[1], &[]);
        let mut machine = machine_in(State::Deferred {
            change: c.clone(),
            after_failure: false,
        });
        let effects = machine.handle(probed(false, false, 1_000));
        assert_eq!(
            effects,
            vec![Effect::StartSync {
                restart_steam: false
            }]
        );
        assert_eq!(
            machine.state(),
            &State::Syncing {
                change: c,
                restart_steam: false
            }
        );
    }

    #[test]
    fn deferred_with_the_same_change_and_steam_open_stays() {
        let c = change(&[1], &[]);
        let state = State::Deferred {
            change: c.clone(),
            after_failure: false,
        };
        let mut machine = machine_in(state.clone());
        assert!(machine.handle(changed(&c, true, false, 300_000)).is_empty());
        assert_eq!(machine.state(), &state);
    }

    #[test]
    fn deferred_with_a_different_change_prompts_again() {
        let deferred = change(&[1], &[]);
        let newer = change(&[1, 2], &[]);
        let mut machine = machine_in(State::Deferred {
            change: deferred,
            after_failure: false,
        });
        let effects = machine.handle(changed(&newer, true, false, 1_000));
        assert_eq!(
            effects,
            vec![Effect::ShowPrompt {
                change: newer.clone(),
                deadline: 61_000
            }]
        );
        assert_eq!(
            machine.state(),
            &State::Countdown {
                change: newer,
                deadline: 61_000
            }
        );
    }

    #[test]
    fn sync_now_from_deferred_with_steam_open_and_no_game_restarts_steam() {
        let c = change(&[1], &[]);
        let mut machine = machine_in(State::Deferred {
            change: c.clone(),
            after_failure: false,
        });
        let effects = machine.handle(Event::SyncNow {
            steam_running: true,
            game_running: false,
        });
        assert_eq!(
            effects,
            vec![Effect::StartSync {
                restart_steam: true
            }]
        );
        assert_eq!(
            machine.state(),
            &State::Syncing {
                change: c,
                restart_steam: true
            }
        );
    }

    #[test]
    fn sync_now_from_waiting_with_steam_closed_syncs_without_restart() {
        let c = change(&[1], &[]);
        let mut machine = machine_in(State::WaitingForGame { change: c.clone() });
        let effects = machine.handle(Event::SyncNow {
            steam_running: false,
            game_running: true,
        });
        assert_eq!(
            effects,
            vec![Effect::StartSync {
                restart_steam: false
            }]
        );
        assert_eq!(
            machine.state(),
            &State::Syncing {
                change: c,
                restart_steam: false
            }
        );
    }

    #[test]
    fn sync_now_while_playing_with_steam_open_stays_and_says_why() {
        let c = change(&[1], &[]);
        for state in [
            State::WaitingForGame { change: c.clone() },
            State::Deferred {
                change: c.clone(),
                after_failure: false,
            },
        ] {
            let mut machine = machine_in(state.clone());
            let effects = machine.handle(Event::SyncNow {
                steam_running: true,
                game_running: true,
            });
            assert_eq!(effects, vec![Effect::Notify(WAITS_FOR_GAME.to_string())]);
            assert_eq!(machine.state(), &state);
        }
    }

    #[test]
    fn a_check_without_changes_returns_every_waiting_state_to_idle() {
        let c = change(&[1], &[]);
        for state in [
            State::WaitingForGame { change: c.clone() },
            State::Countdown {
                change: c.clone(),
                deadline: 60_000,
            },
            State::Deferred {
                change: c.clone(),
                after_failure: false,
            },
        ] {
            let mut machine = machine_in(state);
            assert_eq!(machine.handle(no_change(1_000)), vec![Effect::HidePrompt]);
            assert_eq!(machine.state(), &State::Idle);
        }
    }

    #[test]
    fn a_blocked_check_leaves_every_state_unchanged() {
        let c = change(&[1], &[]);
        for state in [
            State::Idle,
            State::WaitingForGame { change: c.clone() },
            State::Countdown {
                change: c.clone(),
                deadline: 60_000,
            },
            State::Deferred {
                change: c.clone(),
                after_failure: false,
            },
            State::Syncing {
                change: c.clone(),
                restart_steam: true,
            },
        ] {
            let mut machine = machine_in(state.clone());
            let effects = machine.handle(Event::Checked {
                result: CheckResult::Blocked {
                    platforms: vec!["Epic".to_string()],
                },
                steam_running: false,
                game_running: false,
                now: 120_000,
            });
            assert!(effects.is_empty());
            assert_eq!(machine.state(), &state);
        }
    }

    #[test]
    fn checks_and_probes_wait_while_syncing() {
        let c = change(&[1], &[]);
        let state = State::Syncing {
            change: c.clone(),
            restart_steam: false,
        };
        let mut machine = machine_in(state.clone());
        assert!(machine
            .handle(changed(&change(&[9], &[]), false, false, 0))
            .is_empty());
        assert!(machine.handle(probed(false, false, 0)).is_empty());
        assert_eq!(machine.state(), &state);
    }

    #[test]
    fn a_finished_sync_goes_idle() {
        let mut machine = machine_in(State::Syncing {
            change: change(&[1], &[]),
            restart_steam: true,
        });
        assert!(machine
            .handle(Event::SyncFinished { error: None })
            .is_empty());
        assert_eq!(machine.state(), &State::Idle);
    }

    #[test]
    fn a_failed_sync_defers_the_same_change_and_notifies() {
        let c = change(&[1], &[]);
        let mut machine = machine_in(State::Syncing {
            change: c.clone(),
            restart_steam: true,
        });
        let effects = machine.handle(Event::SyncFinished {
            error: Some("disk full".to_string()),
        });
        assert_eq!(
            effects,
            vec![Effect::Notify(
                "Import failed: disk full. Pick Sync now in the tray to try again.".to_string()
            )]
        );
        assert_eq!(
            machine.state(),
            &State::Deferred {
                change: c,
                after_failure: true
            }
        );
    }

    #[test]
    fn failed_import_is_not_retried_when_steam_is_closed() {
        let c = change(&[1], &[]);
        let state = State::Deferred {
            change: c.clone(),
            after_failure: true,
        };
        let mut machine = machine_in(state.clone());
        assert!(machine.handle(probed(false, false, 5_000)).is_empty());
        assert!(machine
            .handle(changed(&c, false, false, 300_000))
            .is_empty());
        assert_eq!(machine.state(), &state);
    }

    #[test]
    fn reset_returns_to_idle_and_closes_an_open_prompt() {
        let mut machine = machine_in(State::Countdown {
            change: change(&[1], &[]),
            deadline: 60_000,
        });
        assert_eq!(machine.reset(), vec![Effect::HidePrompt]);
        assert_eq!(machine.state(), &State::Idle);
    }

    #[test]
    fn waiting_with_steam_closed_imports_even_while_a_game_runs() {
        let c = change(&[1], &[]);
        let mut machine = machine_in(State::WaitingForGame { change: c.clone() });
        let effects = machine.handle(probed(false, true, 5_000));
        assert_eq!(
            effects,
            vec![Effect::StartSync {
                restart_steam: false
            }]
        );
        assert_eq!(
            machine.state(),
            &State::Syncing {
                change: c,
                restart_steam: false
            }
        );
    }

    /// Steam is restarted for an import; a sync that "worked" but left Steam unchanged
    /// would otherwise restart it again at every check.
    #[test]
    fn the_same_change_after_a_successful_import_is_not_imported_again() {
        let c = change(&[1], &[]);
        let mut machine = machine_in(State::Syncing {
            change: c.clone(),
            restart_steam: true,
        });
        assert!(machine
            .handle(Event::SyncFinished { error: None })
            .is_empty());

        let effects = machine.handle(changed(&c, true, false, 300_000));
        assert_eq!(
            effects,
            vec![Effect::Notify(UNCHANGED_BY_IMPORT.to_string())]
        );
        assert_eq!(
            machine.state(),
            &State::Deferred {
                change: c.clone(),
                after_failure: true
            }
        );
        // Not retried when Steam is found closed, nor notified again.
        assert!(machine.handle(probed(false, false, 305_000)).is_empty());
        assert!(machine.handle(changed(&c, true, false, 600_000)).is_empty());
    }

    #[test]
    fn a_different_change_after_an_import_starts_normally() {
        let first = change(&[1], &[]);
        let second = change(&[1, 2], &[]);
        let mut machine = machine_in(State::Syncing {
            change: first,
            restart_steam: true,
        });
        machine.handle(Event::SyncFinished { error: None });
        assert_eq!(
            machine.handle(changed(&second, true, false, 1_000)),
            vec![Effect::ShowPrompt {
                change: second,
                deadline: 61_000
            }]
        );
    }

    #[test]
    fn a_check_without_changes_forgets_the_last_import() {
        let c = change(&[1], &[]);
        let mut machine = machine_in(State::Syncing {
            change: c.clone(),
            restart_steam: true,
        });
        machine.handle(Event::SyncFinished { error: None });
        assert!(machine.handle(no_change(1_000)).is_empty());
        // The same games appearing again later is a new change.
        assert_eq!(
            machine.handle(changed(&c, true, false, 2_000)),
            vec![Effect::ShowPrompt {
                change: c,
                deadline: 62_000
            }]
        );
    }

    #[test]
    fn install_while_playing_then_stop_then_countdown_then_sync() {
        let c = change(&[7], &[]);
        let mut machine = Machine::new(COUNTDOWN);

        assert_eq!(
            machine.handle(changed(&c, true, true, 0)),
            vec![Effect::Notify(playing_text(&c))]
        );
        assert!(machine.handle(probed(true, true, 5_000)).is_empty());
        assert!(machine.handle(changed(&c, true, true, 300_000)).is_empty());
        assert_eq!(
            machine.handle(probed(true, false, 305_000)),
            vec![Effect::ShowPrompt {
                change: c.clone(),
                deadline: 365_000
            }]
        );
        assert!(machine.handle(probed(true, false, 335_000)).is_empty());
        assert_eq!(
            machine.handle(probed(true, false, 365_000)),
            vec![
                Effect::HidePrompt,
                Effect::StartSync {
                    restart_steam: true
                }
            ]
        );
        assert!(machine
            .handle(Event::SyncFinished { error: None })
            .is_empty());
        assert_eq!(machine.state(), &State::Idle);
    }
}
