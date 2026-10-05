//! Automatic sync: BoilR waits in the tray, notices when the games to import changed and
//! imports them, never restarting Steam under a running game. Only Windows starts it;
//! elsewhere the code is compiled and unit-tested but never run, hence the allow.
#![cfg_attr(not(windows), allow(dead_code))]

pub mod detect;
pub mod executor;
pub mod live;
pub mod machine;
pub mod probes;
#[cfg(windows)]
mod probes_windows;
pub mod runner;

pub use runner::AutosyncHandle;

#[cfg(windows)]
mod windows_app;

pub const AVAILABLE: bool = cfg!(windows);
pub const BACKGROUND_ARG: &str = "--background";
pub const MAIN_LABEL: &str = "main";
pub const PROMPT_LABEL: &str = "autosync-prompt";
pub const PROMPT_WIDTH: f64 = 360.0;
pub const PROMPT_HEIGHT: f64 = 150.0;
const PROMPT_MARGIN: f64 = 16.0;
/// Room for a bottom taskbar, which the monitor size includes.
const TASKBAR_ALLOWANCE: f64 = 56.0;

/// Started by Windows at sign-in (`--background`) with autosync on: stay in the tray.
/// A stale autostart entry with autosync off still shows the window.
pub fn start_hidden(args: &[String], available: bool, enabled: bool) -> bool {
    available && enabled && args.iter().any(|arg| arg == BACKGROUND_ARG)
}

/// Top-left corner of the prompt, in logical pixels, for a monitor at (x, y) of the
/// given logical size.
pub fn prompt_corner(x: f64, y: f64, width: f64, height: f64) -> (f64, f64) {
    (
        (x + width - PROMPT_WIDTH - PROMPT_MARGIN).max(x),
        (y + height - PROMPT_HEIGHT - PROMPT_MARGIN - TASKBAR_ALLOWANCE).max(y),
    )
}

pub fn show_main<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    use tauri::Manager;
    if let Some(window) = app.get_webview_window(MAIN_LABEL) {
        let _ = window.unminimize();
        if let Err(err) = window.show() {
            eprintln!("Could not show the BoilR window: {err}");
        }
        let _ = window.set_focus();
    }
}

/// With autosync running, closing the main window hides it; the tray's Quit exits.
pub fn on_window_event(window: &tauri::Window, event: &tauri::WindowEvent) {
    use tauri::Manager;
    if window.label() != MAIN_LABEL {
        return;
    }
    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
        let keeps_running = window
            .try_state::<AutosyncHandle>()
            .is_some_and(|handle| handle.keeps_running());
        if keeps_running {
            api.prevent_close();
            if let Err(err) = window.hide() {
                eprintln!("Could not hide the BoilR window: {err}");
            }
        }
    }
}

#[cfg(windows)]
pub fn with_plugins(builder: tauri::Builder<tauri::Wry>) -> tauri::Builder<tauri::Wry> {
    windows_app::with_plugins(builder)
}

#[cfg(not(windows))]
pub fn with_plugins(builder: tauri::Builder<tauri::Wry>) -> tauri::Builder<tauri::Wry> {
    builder
}

#[cfg(windows)]
pub fn setup(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    windows_app::setup(app)
}

#[cfg(not(windows))]
pub fn setup(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    use tauri::Manager;
    app.manage(AutosyncHandle::unavailable());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn background_start_is_hidden_only_when_autosync_runs() {
        let background = args(&["boilr.exe", "--background"]);
        assert!(start_hidden(&background, true, true));
        assert!(
            !start_hidden(&background, true, false),
            "stale autostart entry: show the window"
        );
        assert!(
            !start_hidden(&background, false, true),
            "no autosync on this OS"
        );
        assert!(!start_hidden(&args(&["boilr.exe"]), true, true));
    }

    #[test]
    fn the_prompt_sits_in_the_bottom_right_corner_above_the_taskbar() {
        let (x, y) = prompt_corner(0.0, 0.0, 1920.0, 1080.0);
        assert_eq!(x, 1920.0 - PROMPT_WIDTH - 16.0);
        assert_eq!(y, 1080.0 - PROMPT_HEIGHT - 16.0 - 56.0);
    }

    #[test]
    fn the_prompt_stays_on_a_tiny_screen() {
        assert_eq!(prompt_corner(100.0, 50.0, 200.0, 100.0), (100.0, 50.0));
    }
}
