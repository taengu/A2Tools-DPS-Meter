//! The tray icon: the meter's way back while it is hidden, locked or out of
//! the way (Hirai, 2026-10-22). Its menu shows or hides the meter, locks or
//! unlocks it, opens Settings and quits; a left click shows or hides the
//! meter (Windows; Linux trays open the menu on any click). Labelled in the
//! meter's language, from the same UI text as Settings.

use tauri::Manager;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

use crate::app::AppState;

const ID: &str = "a2tools-tray";

/// Whether the tray icon is there: always a way back to the meter (a click
/// shows it), so a meter the player hid stays hidden (`app::keep_something_on_screen`).
pub fn present(app: &tauri::AppHandle) -> bool {
    app.tray_by_id(ID).is_some()
}

/// Add the tray icon, where the desktop has a tray.
pub fn spawn(app: &tauri::AppHandle) {
    if !crate::platform::window::tray_supported() {
        tracing::info!("Tray: no tray here (no AppIndicator library), none added");
        return;
    }
    let Some(icon) = app.default_window_icon().cloned() else { return };
    let built = menu(app).and_then(|menu| {
        TrayIconBuilder::with_id(ID)
            .icon(icon)
            .tooltip("A2Tools DPS Meter")
            .menu(&menu)
            .show_menu_on_left_click(false)
            .on_menu_event(|app, event| match event.id().as_ref() {
                "toggle" => crate::app::toggle_meter(app),
                "lock" => crate::app::toggle_overlay_lock(app),
                "settings" => {
                    let app = app.clone();
                    tauri::async_runtime::spawn(async move {
                        if let Err(e) = crate::app::open_settings_window(app).await {
                            tracing::warn!("Tray: could not open Settings: {e}");
                        }
                    });
                }
                "quit" => crate::app::quit_app(app.clone()),
                _ => {}
            })
            .on_tray_icon_event(|tray, event| {
                if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                    crate::app::toggle_meter(tray.app_handle());
                }
            })
            .build(app)
    });
    if let Err(e) = built {
        tracing::warn!("Tray: could not add the icon: {e}");
    }
}

/// Label the menu again, in the language just chosen.
pub fn relabel(app: &tauri::AppHandle) {
    if let (Some(tray), Ok(menu)) = (app.tray_by_id(ID), menu(app)) {
        let _ = tray.set_menu(Some(menu));
    }
}

fn menu(app: &tauri::AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let text = labels(app);
    Menu::with_items(
        app,
        &[
            &MenuItem::with_id(app, "toggle", &text[0], true, None::<&str>)?,
            &MenuItem::with_id(app, "lock", &text[1], true, None::<&str>)?,
            &MenuItem::with_id(app, "settings", &text[2], true, None::<&str>)?,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(app, "quit", &text[3], true, None::<&str>)?,
        ],
    )
}

/// The menu's labels (show/hide, lock, Settings, quit): the keybinds' own
/// names and Settings' title, in the meter's language, else English.
fn labels(app: &tauri::AppHandle) -> [String; 4] {
    let state = app.try_state::<AppState>();
    let lang = state.as_ref().and_then(|s| s.settings.get("dpsMeter.language")).unwrap_or_else(|| "en".into());
    let data_dir = state.as_ref().and_then(|s| s.i18n_data_dir.clone());
    let load = |lang: &str| -> Option<serde_json::Value> {
        // Only a language name, never a path.
        if !lang.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return None;
        }
        let path = data_dir.as_ref()?.join("i18n").join("ui").join(format!("{lang}.json"));
        serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
    };
    let (ui, en) = (load(&lang), load("en"));
    let text = |path: &[&str], fallback: &str| -> String {
        [ui.as_ref(), en.as_ref()]
            .into_iter()
            .flatten()
            .find_map(|v| path.iter().fold(v, |v, k| &v[*k]).as_str().map(str::to_string))
            .unwrap_or_else(|| fallback.to_string())
    };
    [
        text(&["settings", "keybind", "toggle"], "Show/Hide window"),
        text(&["settings", "keybind", "lock"], "Lock/unlock overlay"),
        text(&["settings", "title"], "Settings"),
        text(&["settings", "quit"], "Quit"),
    ]
}
