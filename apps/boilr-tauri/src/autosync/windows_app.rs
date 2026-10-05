//! The Windows shell around the runner: tray icon, single instance, start with Windows,
//! toasts and the countdown window. Only compiled on Windows (CI's tauri_Windows job); the
//! logic it drives is tested on every platform.

use std::sync::{Arc, Mutex};

use boilr_core::settings::Settings;
use tauri::{
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    AppHandle, Builder, Manager, WebviewUrl, WebviewWindowBuilder, Wry,
};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};
use tauri_plugin_notification::NotificationExt;
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};

use super::{
    first_check_delay,
    live::LiveWork,
    probes::create_probes,
    prompt_corner,
    runner::{
        run_loop, AutosyncHandle, AutosyncStatus, AutosyncUi, Runner, RunnerInput, SharedStatus,
    },
    show_main, BACKGROUND_ARG, PROMPT_HEIGHT, PROMPT_LABEL, PROMPT_WIDTH,
};

const TRAY_ID: &str = "boilr-autosync";
const MENU_OPEN: &str = "open";
const MENU_SYNC: &str = "sync_now";
const MENU_PAUSE: &str = "pause";
const MENU_QUIT: &str = "quit";

pub fn with_plugins(builder: Builder<Wry>) -> Builder<Wry> {
    builder
        // First plugin, so a second launch is caught before anything else starts.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main(app)
        }))
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec![BACKGROUND_ARG]),
        ))
        .plugin(tauri_plugin_notification::init())
}

pub fn setup(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let handle = app.handle().clone();
    let Some(probes) = create_probes() else {
        app.manage(AutosyncHandle::unavailable());
        return Ok(());
    };
    let (inputs, receiver) = unbounded_channel();
    let pause_item = match build_tray(&handle, inputs.clone()) {
        Ok(pause_item) => pause_item,
        Err(err) => {
            // Without a tray nothing could bring a hidden window back: autosync stays off
            // and the main window behaves as on the other systems.
            eprintln!("Could not create the tray icon, autosync is off: {err}");
            app.manage(AutosyncHandle::unavailable());
            return Ok(());
        }
    };
    // Seeded from the settings so closing the window hides it even before the first
    // check (which can take a while) has published anything.
    let enabled = Settings::new()
        .map(|settings| settings.autosync.enabled)
        .unwrap_or(false);
    let status: SharedStatus = Arc::new(Mutex::new(AutosyncStatus {
        available: true,
        enabled,
        ..AutosyncStatus::default()
    }));

    let ui = Arc::new(TauriUi {
        app: handle.clone(),
        pause_item,
    });
    let runner = Runner::new(Arc::new(LiveWork), probes.clone(), ui, status.clone());
    app.manage(AutosyncHandle {
        inputs: Some(inputs),
        status,
        probes: Some(probes),
    });
    let args: Vec<String> = std::env::args().collect();
    tauri::async_runtime::spawn(run_loop(runner, receiver, first_check_delay(&args)));
    Ok(())
}

/// Builds the tray and returns its pause item, whose text follows the pause state.
fn build_tray(
    app: &AppHandle,
    inputs: UnboundedSender<RunnerInput>,
) -> tauri::Result<MenuItem<Wry>> {
    let open = MenuItem::with_id(app, MENU_OPEN, "Open BoilR", true, None::<&str>)?;
    let sync_now = MenuItem::with_id(app, MENU_SYNC, "Sync now", true, None::<&str>)?;
    let pause = MenuItem::with_id(app, MENU_PAUSE, "Pause autosync", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, MENU_QUIT, "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &sync_now, &pause, &quit])?;

    let mut tray = TrayIconBuilder::with_id(TRAY_ID)
        .menu(&menu)
        .tooltip("BoilR")
        .on_menu_event(move |app, event| {
            let input = match event.id().as_ref() {
                MENU_OPEN => {
                    show_main(app);
                    return;
                }
                MENU_QUIT => {
                    // Waits for a running import: see `on_run_event`.
                    app.exit(0);
                    return;
                }
                MENU_SYNC => RunnerInput::SyncNow,
                MENU_PAUSE => RunnerInput::TogglePause,
                _ => return,
            };
            if inputs.send(input).is_err() {
                eprintln!("Autosync has stopped; the tray menu has nothing to talk to");
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(pause)
}

struct TauriUi {
    app: AppHandle,
    pause_item: MenuItem<Wry>,
}

impl AutosyncUi for TauriUi {
    fn show_prompt(&self) {
        // An open prompt reads the new text and deadline by itself.
        if self.app.get_webview_window(PROMPT_LABEL).is_some() {
            return;
        }
        if let Err(err) = open_prompt(&self.app) {
            eprintln!("Could not open the autosync prompt: {err}");
        }
    }

    fn hide_prompt(&self) {
        if let Some(window) = self.app.get_webview_window(PROMPT_LABEL) {
            if let Err(err) = window.destroy() {
                eprintln!("Could not close the autosync prompt: {err}");
            }
        }
    }

    fn notify(&self, text: &str) {
        notify(&self.app, text);
    }

    fn set_tooltip(&self, text: &str) {
        if let Some(tray) = self.app.tray_by_id(TRAY_ID) {
            if let Err(err) = tray.set_tooltip(Some(text)) {
                eprintln!("Could not update the tray tooltip: {err}");
            }
        }
    }

    fn set_paused(&self, paused: bool) {
        let label = if paused {
            "Resume autosync"
        } else {
            "Pause autosync"
        };
        if let Err(err) = self.pause_item.set_text(label) {
            eprintln!("Could not update the tray menu: {err}");
        }
    }

    fn set_autostart(&self, enabled: bool) {
        let autostart = self.app.autolaunch();
        let result = match autostart.is_enabled() {
            Ok(current) if current == enabled => Ok(()),
            _ if enabled => autostart.enable(),
            _ => autostart.disable(),
        };
        if let Err(err) = result {
            eprintln!("Could not change start with Windows: {err}");
        }
    }

    fn exit(&self) {
        self.app.exit(0);
    }
}

pub fn notify(app: &AppHandle, text: &str) {
    if let Err(err) = app
        .notification()
        .builder()
        .title("BoilR")
        .body(text)
        .show()
    {
        eprintln!("Could not show a notification: {err}");
    }
}

fn open_prompt(app: &AppHandle) -> tauri::Result<()> {
    let (x, y) = match app.primary_monitor() {
        Ok(Some(monitor)) => {
            let scale = monitor.scale_factor();
            let position = monitor.position();
            let size = monitor.size();
            prompt_corner(
                f64::from(position.x) / scale,
                f64::from(position.y) / scale,
                f64::from(size.width) / scale,
                f64::from(size.height) / scale,
            )
        }
        _ => (0.0, 0.0),
    };
    WebviewWindowBuilder::new(app, PROMPT_LABEL, WebviewUrl::App("index.html".into()))
        .title("BoilR")
        .inner_size(PROMPT_WIDTH, PROMPT_HEIGHT)
        .position(x, y)
        .resizable(false)
        .decorations(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .focused(false)
        .build()?;
    Ok(())
}
