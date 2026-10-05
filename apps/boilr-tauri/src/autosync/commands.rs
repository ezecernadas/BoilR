//! Commands the prompt window calls. The runner probes Steam and the game itself when it
//! receives `RestartNow`, so the commands only forward the answer.

use tauri::State;

use super::runner::{lock_status, AutosyncHandle, AutosyncStatus, PromptInfo, RunnerInput};

/// Text and deadline for the countdown window; `None` when no prompt is open.
#[tauri::command]
pub fn autosync_prompt_info(handle: State<'_, AutosyncHandle>) -> Option<PromptInfo> {
    lock_status(&handle.status).prompt.clone()
}

#[tauri::command]
pub fn autosync_restart_now(handle: State<'_, AutosyncHandle>) -> Result<(), String> {
    handle.send(RunnerInput::RestartNow)
}

#[tauri::command]
pub fn autosync_not_now(handle: State<'_, AutosyncHandle>) -> Result<(), String> {
    handle.send(RunnerInput::NotNow)
}

/// Lets the prompt close itself when it has nothing to show. A command rather than the
/// window API, which would need a capability; only the prompt window may be closed.
#[tauri::command]
pub fn autosync_close_prompt<R: tauri::Runtime>(
    window: tauri::WebviewWindow<R>,
) -> Result<(), String> {
    if window.label() != super::PROMPT_LABEL {
        return Err("Only the autosync prompt can be closed this way".to_string());
    }
    window.destroy().map_err(|err| err.to_string())
}

/// Status for the settings page, with fresh Steam and game readings so the diagnostics
/// readout can be checked against a running game.
#[tauri::command]
pub async fn autosync_status(handle: State<'_, AutosyncHandle>) -> Result<AutosyncStatus, String> {
    let mut status = lock_status(&handle.status).clone();
    if let Some(probes) = handle.probes.clone() {
        let (steam_running, signals) = tauri::async_runtime::spawn_blocking(move || {
            (probes.steam_running(), probes.game_signals())
        })
        .await
        .map_err(|err| err.to_string())?;
        status.steam_running = Some(steam_running);
        status.game_signals = signals;
    }
    Ok(status)
}
