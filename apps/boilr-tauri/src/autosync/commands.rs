//! Commands the prompt window calls. The runner probes Steam and the game itself when it
//! receives `RestartNow`, so the commands only forward the answer.

use tauri::State;

use super::runner::{lock_status, AutosyncHandle, PromptInfo, RunnerInput};

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
