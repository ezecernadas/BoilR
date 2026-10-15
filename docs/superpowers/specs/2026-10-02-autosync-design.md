# Automatic sync (autosync) — design

Date: 2026-10-02
Status: awaiting review
Scope: personal Windows fork of BoilR, built on the Tauri app (`apps/boilr-tauri`).

## Goal

BoilR imports games only when the user clicks Import. This change makes the Tauri
app stay in the system tray, notice when the set of importable games changed, and
import them without the user opening BoilR, while never restarting Steam under a
running game.

Success: installing or uninstalling a game in any enabled launcher ends up
reflected in Steam with at most one click from the user, and Steam is never
closed while a game is running.

Out of scope: the UI redesign (separate sub-project), the egui app, Linux and
macOS support for autosync, upstreaming.

## Behaviour (approved)

A check runs every `interval_minutes` (default 5). When it finds a change:

| Situation | Action |
|---|---|
| Steam is closed | Import silently. Steam is not started. |
| Steam is open, no game running | Show a prompt window: "N new games. Steam restarts in 60 s", with **Restart now** and **Not now**. At zero: stop Steam, import, start Steam. |
| A game is running | Show one plain Windows notification: "N new games, they will be imported when you stop playing". No restart. |

- **Not now** defers the import. It runs when Steam is next found closed, when the
  user picks "Sync now" in the tray menu, or it prompts again when the set of
  changes becomes different from the deferred one.
- When the game stops and an import is pending, the 60 s prompt starts.
- A change is an added game or a removed game.
- The app starts with Windows, hidden in the tray. This can be turned off.

## What counts as a change

`plan_sync` cannot be used as the diff: it lists every BoilR-owned shortcut as a
removal, because a sync removes all of them and re-adds the current set.

The check instead compares two sets of app ids, with no state file:

- **desired**: the app ids `prepare_additions` produces from the platform
  snapshots (after blacklist and renames).
- **current**: the app ids of BoilR-owned shortcuts in Steam's `shortcuts.vdf`,
  for every Steam user (`is_boilr_shortcut`).

`added = desired − current`, `removed = current − desired`. There is a change
when either is non-empty. The pair is the **change set**; its identity is used to
tell a deferred change from a new one.

### Failing launchers

A sync removes every BoilR-owned shortcut and re-adds only what the launchers
that answered returned. If an enabled launcher returns an error, its games would
be deleted from Steam.

Rule: **when any enabled platform snapshot has an error, the check reports
"blocked" and autosync does nothing in that cycle.** The tray tooltip and the
settings page show which platform is failing, so a permanently broken launcher
can be disabled by the user. Manual import keeps today's behaviour.

This is stricter than "ignore that platform's changes" from the design
discussion: ignoring the changes is not enough, since running the sync is what
deletes the shortcuts.

## Components

All new backend code lives in `apps/boilr-tauri/src/autosync/`.

### `machine.rs` — state machine (pure)

No Tauri, no OS calls, no clock: time comes in with the event. This is where the
behaviour table lives and it is fully unit-tested.

States: `Idle`, `WaitingForGame { change }`, `Countdown { change, deadline }`,
`Deferred { change }`, `Syncing { restart_steam }`.

Events: `Checked { result, steam_running, game_running, now }`,
`Probed { steam_running, game_running, now }`, `RestartNow`, `NotNow`,
`SyncNow`, `SyncFinished { ok }`.

Effects returned to the runner: `ShowPrompt { change, deadline }`, `HidePrompt`,
`Notify(text)`, `StartSync { restart_steam }`.

Transitions:

- `Idle` + change, Steam closed → `Syncing { restart_steam: false }`.
- `Idle` + change, Steam open, no game → `Countdown`, `ShowPrompt`.
- `Idle` + change, game running → `WaitingForGame`, `Notify` once.
- `WaitingForGame` + game stopped → `Countdown` (Steam open) or `Syncing` (closed).
- `Countdown` + deadline passed → `Syncing { restart_steam: true }`; if a game
  started meanwhile → `WaitingForGame` without a second notification.
- `Countdown` + `RestartNow` → `Syncing { restart_steam: true }`.
- `Countdown` + `NotNow` → `Deferred`, `HidePrompt`.
- `Deferred` + Steam closed → `Syncing { restart_steam: false }`.
- `Deferred` + a different change set → treated as `Idle` + change.
- `Deferred` or `WaitingForGame` + `SyncNow` → `Syncing`, restart only if Steam is open and no game is running; otherwise stays and notifies why.
- Any waiting state + check with no change → `Idle`, `HidePrompt`.
- Any state + blocked check → unchanged.
- `Syncing` + `SyncFinished { ok: true }` → `Idle`.
- `Syncing` + `SyncFinished { ok: false }` → `Deferred` with the same change set,
  plus `Notify` with the error, so a failing import is not retried in a loop.

### `detect.rs` — change detection

`detect(snapshots, settings, renames) -> CheckResult` where `CheckResult` is
`NoChange`, `Changed(ChangeSet)` or `Blocked { platforms }`. Takes the snapshots
as input so it is testable without launchers. `prepare_additions` is reused;
reading the current owned ids is a small new function next to `collect_removals`.

### `probes.rs` — questions to the OS

```rust
trait Probes {
    fn steam_running(&self) -> bool;
    fn game_running(&self) -> bool;
}
```

- `steam_running`: reuse the `sysinfo` lookup `restarter.rs` already does
  (make it public in `boilr-core`).
- `game_running` is true when **any** of these signals is true:
  1. A process whose executable path is inside a known game directory: the
     install folders of imported games. Steam library `steamapps/common`
     folders are not used: tools such as Wallpaper Engine run from there all
     day without Steam reporting them as running, so Steam games rely on
     signal 2. Directories under `%WinDir%` are excluded, because Xbox shortcuts
     launch through `explorer.exe`. The Xbox platform gains an accessor for
     `install_location` so its games are covered.
  2. Steam reports a running app (`HKCU\Software\Valve\Steam\RunningAppID` ≠ 0).
  3. Windows reports a full-screen app (`SHQueryUserNotificationState`).

Processes are matched by path, never by PID (repo rule).

**Unverified.** None of the three signals has been tested; signal 2 for
non-Steam shortcuts and the install paths of Xbox games are the main doubts. The
first task on Windows is a diagnostics readout (below) to check them against
real games before the restart logic is trusted. A false positive only delays an
import; a false negative still goes through the 60 s prompt.

The Windows implementation is behind `#[cfg(windows)]`. On other systems autosync
is unavailable and the setting is hidden.

### `runner.rs` — the loop

A Tokio task owning the machine. It runs the full check every `interval_minutes`,
and a cheap `Probed` tick every 5 s while the state is not `Idle` (needed for the
countdown and to notice the game stopping). It turns effects into calls on the
tray, the prompt window, notifications and the executor. `paused` skips checks.

### Executor

`run_autosync(restart_steam)`:

1. If `restart_steam`: `ensure_steam_stopped`.
2. `perform_full_sync`.
3. If `restart_steam`: `ensure_steam_started`, **whatever step 2 returned**.

Steam's stop/start here is driven by the machine, not by the `stop_steam` /
`start_steam` settings, which keep governing manual imports. `perform_full_sync`
gets a parameter to skip its own stop/start so it is not done twice.

### Tray, window and single instance

- `TrayIconBuilder` with a menu: Open BoilR, Sync now, Pause autosync, Quit. The
  tooltip shows the state ("Up to date", "2 games pending", "Blocked: Epic").
- With autosync enabled, closing the main window hides it instead of quitting.
- `tauri-plugin-single-instance`: launching BoilR again shows the existing window.
- `tauri-plugin-autostart` registers the app with a `--background` argument;
  with it the main window starts hidden.

### Prompt window and notification

- The countdown is a second, small Tauri window (`autosync-prompt`): no
  decorations, always on top, bottom-right corner, not focused on open. It is a
  React view with the countdown and the two buttons, which call the
  `autosync_restart_now` and `autosync_not_now` commands. Windows toast
  notifications cannot carry buttons in Tauri (the Actions API is mobile only).
- The "you are playing" message is a plain toast via `tauri-plugin-notification`.

### Settings

New `AutosyncSettings` in `boilr-core` following the `SteamSettings` pattern,
with a block in `defaultconfig.toml` (required: there is no `#[serde(default)]`):

```toml
[autosync]
enabled = false
interval_minutes = 5
countdown_seconds = 60
start_with_windows = true
```

`"autosync"` is added to the section blacklist in `load_setting_sections`. The
settings page gets an Autosync section with these four values and a
**diagnostics readout**: Steam running yes/no, game running yes/no and which
signal fired, last check time and result. `enabled` defaults to false so the
feature is opt-in.

## Error handling

- Failing launcher → blocked cycle, visible in tray and settings.
- Import error → Steam restarted if it was stopped, error notification, state
  `Deferred`.
- Steam path not found → `ensure_steam_started` does nothing today; the executor
  reports it in the notification instead of staying silent.
- Settings unreadable → the check is skipped and logged; the loop keeps running.

## Testing

On Linux, test-first:

- `machine.rs`: one test per transition above, plus the full scenarios (install
  while playing → stop playing → countdown → sync).
- `detect.rs`: added, removed, no change, blocked by a failing platform,
  blacklisted game not counted.
- Executor: Steam is started again when the import fails (fake probes and sync).

Checks: `cargo test`, `cargo clippy --all-targets` and `npm run build` in
`apps/boilr-tauri`.

On Windows, by hand, with the diagnostics readout:

1. Signals: Steam game, non-Steam shortcut, Xbox game, Epic game, each windowed
   and full screen.
2. Install a game with Steam closed → imported silently.
3. Install with Steam open → prompt; Restart now; Not now; let it reach zero.
4. Install while playing → toast only; stop playing → prompt.
5. Uninstall a game → removed from Steam through the same flow.
6. Close a launcher so it errors → blocked, nothing deleted.
7. Reboot → app in tray, window hidden; launching BoilR shows the window.

`#[cfg(windows)]` code does not compile on Linux, so it is checked by the
`tauri_Windows` CI job and by these manual steps.

## Delivery

Work happens on branch `feat/autosync` of a local clone. Getting a Windows build
without installing Rust and Node on the Windows PC needs a GitHub fork with
Actions producing the executable; creating that fork is a separate decision.
When `Cargo.lock` or `package-lock.json` change, `flatpak/update-sources.sh`
must be run to keep the repo's lock-sync CI check green.
