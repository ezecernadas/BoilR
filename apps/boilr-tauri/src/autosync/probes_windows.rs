//! Windows answers for the probes. Only compiled on Windows: CI's tauri_Windows job is the
//! only build that checks this file.

use std::{path::PathBuf, sync::RwLock};

use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
use windows_sys::Win32::UI::Shell::{
    SHQueryUserNotificationState, QUNS_BUSY, QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN,
};
use winreg::{enums::HKEY_CURRENT_USER, RegKey};

use super::probes::{is_in_game_dir, GameSignal, Probes};

pub struct WindowsProbes {
    dirs: RwLock<Vec<PathBuf>>,
    windows_dir: PathBuf,
}

impl Default for WindowsProbes {
    fn default() -> Self {
        let windows_dir = std::env::var_os("WinDir")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        Self {
            dirs: RwLock::new(Vec::new()),
            windows_dir,
        }
    }
}

impl WindowsProbes {
    fn process_in_game_dir(&self) -> bool {
        let dirs = match self.dirs.read() {
            Ok(dirs) => dirs.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        };
        if dirs.is_empty() {
            return false;
        }
        let mut system = System::new();
        system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing().with_exe(UpdateKind::OnlyIfNotSet),
        );
        system
            .processes()
            .values()
            .filter_map(|process| process.exe())
            .any(|exe| is_in_game_dir(exe, &dirs, &self.windows_dir))
    }

    fn steam_reports_running_app() -> bool {
        RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey(r"Software\Valve\Steam")
            .and_then(|key| key.get_value::<u32, _>("RunningAppID"))
            .map(|app_id| app_id != 0)
            .unwrap_or(false)
    }

    fn full_screen_app() -> bool {
        let mut state = 0;
        // SAFETY: the call writes one QUERY_USER_NOTIFICATION_STATE into `state`, a live
        // local for the whole call.
        let result = unsafe { SHQueryUserNotificationState(&mut state) };
        result >= 0
            && matches!(
                state,
                QUNS_BUSY | QUNS_RUNNING_D3D_FULL_SCREEN | QUNS_PRESENTATION_MODE
            )
    }
}

impl Probes for WindowsProbes {
    fn steam_running(&self) -> bool {
        boilr_core::steam::is_steam_running()
    }

    fn game_signals(&self) -> Vec<GameSignal> {
        let mut signals = Vec::new();
        if self.process_in_game_dir() {
            signals.push(GameSignal::GameFolderProcess);
        }
        if Self::steam_reports_running_app() {
            signals.push(GameSignal::SteamRunningApp);
        }
        if Self::full_screen_app() {
            signals.push(GameSignal::FullScreen);
        }
        signals
    }

    fn set_game_dirs(&self, dirs: Vec<PathBuf>) {
        match self.dirs.write() {
            Ok(mut current) => *current = dirs,
            Err(poisoned) => *poisoned.into_inner() = dirs,
        }
    }
}
