//! Questions autosync asks the OS: is Steam running, is a game running. Processes are
//! matched by executable path, never by PID.

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::Arc,
};

use serde::{Deserialize, Serialize};
use steam_shortcuts_util::shortcut::ShortcutOwned;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GameSignal {
    /// A process runs from a Steam library or an imported game's folder.
    GameFolderProcess,
    /// `HKCU\Software\Valve\Steam\RunningAppID` is not 0.
    SteamRunningApp,
    /// Windows reports a full-screen app (`SHQueryUserNotificationState`).
    FullScreen,
}

pub trait Probes: Send + Sync {
    fn steam_running(&self) -> bool;
    /// Every signal that says a game is running; empty when none does.
    fn game_signals(&self) -> Vec<GameSignal>;
    /// Folders whose programs count as games, refreshed by every full check.
    fn set_game_dirs(&self, dirs: Vec<PathBuf>);
}

#[cfg(windows)]
pub fn create_probes() -> Option<Arc<dyn Probes>> {
    Some(Arc::new(super::probes_windows::WindowsProbes::default()))
}

/// Autosync only runs on Windows.
#[cfg(not(windows))]
pub fn create_probes() -> Option<Arc<dyn Probes>> {
    None
}

/// Platforms (by `code_name`) whose shortcuts start the launcher from the launcher's own
/// folder. The launcher keeps running, so its folder would make "a game is running"
/// permanently true.
const LAUNCHER_PLATFORMS: [&str; 3] = ["uplay", "amazon", "playnite"];

/// Epic games that need the launcher (`safe_launch`, managed, DLC) start it with this URL
/// from the launcher's folder; the others start their own exe from the install folder.
const EPIC_LAUNCHER_URL: &str = "com.epicgames.launcher://";

/// The shortcut's start folder when it is the game's own folder, `None` when the shortcut
/// starts a launcher from the launcher's folder.
pub fn game_start_dir(code_name: &str, shortcut: &ShortcutOwned) -> Option<String> {
    let through_launcher = LAUNCHER_PLATFORMS.contains(&code_name)
        || shortcut.launch_options.contains(EPIC_LAUNCHER_URL);
    (!through_launcher).then(|| shortcut.start_dir.clone())
}

/// Lower case, `\` separators, no repeated or trailing separators, no surrounding quotes.
fn normalize(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.trim().trim_matches('"').chars() {
        let ch = if ch == '/' { '\\' } else { ch };
        if ch == '\\' && out.ends_with('\\') {
            continue;
        }
        out.extend(ch.to_lowercase());
    }
    while out.ends_with('\\') {
        out.pop();
    }
    out
}

/// A drive root or an empty path would make every program look like a game.
fn is_specific_dir(normalized: &str) -> bool {
    normalized.trim_start_matches('\\').contains('\\')
}

fn starts_with_dir(path: &str, dir: &str) -> bool {
    path == dir
        || path
            .strip_prefix(dir)
            .is_some_and(|rest| rest.starts_with('\\'))
}

/// Whether `exe` lives under one of `dirs`. Programs under the Windows folder never count:
/// Xbox shortcuts launch through `explorer.exe`.
pub fn is_in_game_dir(exe: &Path, dirs: &[PathBuf], windows_dir: &Path) -> bool {
    let exe = normalize(&exe.to_string_lossy());
    if starts_with_dir(&exe, &normalize(&windows_dir.to_string_lossy())) {
        return false;
    }
    dirs.iter()
        .any(|dir| starts_with_dir(&exe, &normalize(&dir.to_string_lossy())))
}

/// Cleans raw folder strings (shortcut start folders, Steam libraries, Xbox install
/// folders) into a de-duplicated list of specific folders.
pub fn game_dirs(raw: impl IntoIterator<Item = String>) -> Vec<PathBuf> {
    let mut seen = BTreeSet::new();
    raw.into_iter()
        .map(|dir| normalize(&dir))
        .filter(|dir| is_specific_dir(dir))
        .filter(|dir| seen.insert(dir.clone()))
        .map(PathBuf::from)
        .collect()
}

#[cfg(test)]
mod tests {
    use steam_shortcuts_util::Shortcut;

    use super::*;

    fn dirs(values: &[&str]) -> Vec<PathBuf> {
        game_dirs(values.iter().map(|value| value.to_string()))
    }

    const WINDIR: &str = r"C:\Windows";

    #[test]
    fn a_program_inside_a_game_folder_matches_regardless_of_case_and_slashes() {
        let dirs = dirs(&[r"C:\Games\Celeste"]);
        assert!(is_in_game_dir(
            Path::new("c:/games/celeste/Celeste.exe"),
            &dirs,
            Path::new(WINDIR)
        ));
    }

    #[test]
    fn a_program_elsewhere_does_not_match() {
        let dirs = dirs(&[r"C:\Games\Celeste"]);
        assert!(!is_in_game_dir(
            Path::new(r"C:\Tools\notepad++.exe"),
            &dirs,
            Path::new(WINDIR)
        ));
    }

    #[test]
    fn a_sibling_folder_with_the_same_prefix_does_not_match() {
        let dirs = dirs(&[r"C:\Games\Foo"]);
        assert!(!is_in_game_dir(
            Path::new(r"C:\Games\FooBar\foo.exe"),
            &dirs,
            Path::new(WINDIR)
        ));
    }

    #[test]
    fn windows_folder_is_never_a_game_folder() {
        let dirs = dirs(&[WINDIR]);
        assert!(!is_in_game_dir(
            Path::new(r"C:\Windows\explorer.exe"),
            &dirs,
            Path::new(WINDIR)
        ));
    }

    #[test]
    fn doubled_backslashes_match() {
        let dirs = dirs(&[r"C:\\Program Files (x86)\\Steam\\steamapps\\common"]);
        assert!(is_in_game_dir(
            Path::new(r"C:\Program Files (x86)\Steam\steamapps\common\Hades\Hades.exe"),
            &dirs,
            Path::new(WINDIR)
        ));
    }

    #[test]
    fn game_dirs_cleans_quotes_roots_and_duplicates() {
        assert_eq!(
            dirs(&[
                r#""C:\Games\Celeste""#,
                r"C:\",
                "",
                r"C:\Games\Celeste\",
                "C:"
            ]),
            vec![PathBuf::from(r"c:\games\celeste")]
        );
    }

    fn shortcut(exe: &str, start_dir: &str, launch_options: &str) -> ShortcutOwned {
        Shortcut::new("0", "Game", exe, start_dir, "", "", launch_options).to_owned()
    }

    #[test]
    fn a_normal_games_folder_is_a_game_folder() {
        let celeste = shortcut(
            r#""C:\Games\Celeste\Celeste.exe""#,
            r#""C:\Games\Celeste""#,
            "",
        );
        assert_eq!(
            game_start_dir("gog", &celeste),
            Some(r#""C:\Games\Celeste""#.to_string())
        );
    }

    #[test]
    fn a_launcher_launched_shortcuts_folder_is_not_a_game_folder() {
        let launcher = r"C:\Program Files (x86)\Ubisoft\Ubisoft Game Launcher";
        let uplay = shortcut(
            &format!(r#""{launcher}\upc.exe""#),
            launcher,
            r#""uplay://launch/635/0""#,
        );
        assert_eq!(game_start_dir("uplay", &uplay), None);

        let amazon_dir = r"C:\Users\me\AppData\Local\Amazon Games\App";
        let amazon = shortcut(
            &format!(r"{amazon_dir}\Amazon Games.exe"),
            amazon_dir,
            "amazon-games://play/amzn1.adg.product.1",
        );
        assert_eq!(game_start_dir("amazon", &amazon), None);

        let playnite_dir = r"C:\Users\me\AppData\Local\Playnite";
        let playnite = shortcut(
            &format!(r"{playnite_dir}\Playnite.DesktopApp.exe"),
            playnite_dir,
            "--hidesplashscreen --start 1234",
        );
        assert_eq!(game_start_dir("playnite", &playnite), None);
    }

    #[test]
    fn only_epic_games_started_through_the_launcher_are_skipped() {
        let launcher_dir = r"C:\Program Files (x86)\Epic Games\Launcher\Portal\Binaries\Win32";
        let through_launcher = shortcut(
            &format!(r"{launcher_dir}\EpicGamesLauncher.exe"),
            launcher_dir,
            "com.epicgames.launcher://apps/ns%3Aitem%3Aname?action=launch&silent=true",
        );
        assert_eq!(game_start_dir("epic_games", &through_launcher), None);

        let direct = shortcut(
            r"C:\Program Files\Epic Games\Hades\Hades.exe",
            r"C:\Program Files\Epic Games\Hades",
            "",
        );
        assert_eq!(
            game_start_dir("epic_games", &direct),
            Some(r"C:\Program Files\Epic Games\Hades".to_string())
        );
    }
}
