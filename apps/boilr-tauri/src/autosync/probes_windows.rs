//! Windows answers for the probes. Only compiled on Windows: CI's tauri_Windows job is the
//! only build that checks this file.

use std::{
    ffi::OsStr,
    path::PathBuf,
    sync::{Mutex, MutexGuard, RwLock},
};

use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
use windows_sys::Win32::UI::Shell::{
    SHQueryUserNotificationState, QUNS_BUSY, QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN,
};
use winreg::{enums::HKEY_CURRENT_USER, RegKey};

use super::probes::{is_in_game_dir, GameSignal, Probes, SignalReading};

const STEAM_BINARY: &str = "steam.exe";

pub struct WindowsProbes {
    dirs: RwLock<Vec<PathBuf>>,
    windows_dir: PathBuf,
    /// Kept between readings, so a process's executable path is read once, not every 5 s.
    system: Mutex<System>,
}

impl Default for WindowsProbes {
    fn default() -> Self {
        let windows_dir = std::env::var_os("WinDir")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        Self {
            dirs: RwLock::new(Vec::new()),
            windows_dir,
            system: Mutex::new(System::new()),
        }
    }
}

impl WindowsProbes {
    /// The process list, refreshed: new processes get their name and executable path,
    /// ended ones are dropped.
    fn processes(&self) -> MutexGuard<'_, System> {
        let mut system = self
            .system
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing().with_exe(UpdateKind::OnlyIfNotSet),
        );
        system
    }

    /// The executable of the first process running from a game folder.
    fn process_in_game_dir(&self) -> Option<PathBuf> {
        let dirs = match self.dirs.read() {
            Ok(dirs) => dirs.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        };
        if dirs.is_empty() {
            return None;
        }
        self.processes()
            .processes()
            .values()
            .filter_map(|process| process.exe())
            .find(|exe| is_in_game_dir(exe, &dirs, &self.windows_dir))
            .map(PathBuf::from)
    }

    /// The app id Steam says is running, when not 0.
    fn steam_running_app() -> Option<u32> {
        RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey(r"Software\Valve\Steam")
            .and_then(|key| key.get_value::<u32, _>("RunningAppID"))
            .ok()
            .filter(|app_id| *app_id != 0)
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
    /// Matched by process name, never by PID.
    fn steam_running(&self) -> bool {
        self.processes()
            .processes_by_name(OsStr::new(STEAM_BINARY))
            .next()
            .is_some()
    }

    fn game_signals(&self) -> Vec<SignalReading> {
        let mut signals = Vec::new();
        if let Some(exe) = self.process_in_game_dir() {
            signals.push(SignalReading {
                signal: GameSignal::GameFolderProcess,
                detail: Some(exe.display().to_string()),
            });
        }
        if let Some(app_id) = Self::steam_running_app() {
            signals.push(SignalReading {
                signal: GameSignal::SteamRunningApp,
                detail: Some(format!("app id {app_id}")),
            });
        }
        if Self::full_screen_app() {
            signals.push(SignalReading {
                signal: GameSignal::FullScreen,
                detail: None,
            });
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
