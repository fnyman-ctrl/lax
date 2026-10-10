//! The menu bar icon and its menu.

use std::sync::{Mutex, PoisonError};
use std::thread;
use std::time::Duration;

use chrono::Local;
use tauri::image::Image;
use tauri::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, Wry, include_image};

use crate::pause::{CaptureState, Pause, PauseLength};
use crate::store::Store;

const TRAY_ID: &str = "lax";

// Template images: macOS recolours them to match the menu bar.
// Drawn from icons/tray/active.svg and paused.svg; run `npm run icons`.
const ICON_RUNNING: Image<'_> = include_image!("icons/tray/active.png");
const ICON_PAUSED: Image<'_> = include_image!("icons/tray/paused.png");

/// How often the menu bar is checked for a timed pause that has run out.
const REFRESH_INTERVAL: Duration = Duration::from_secs(10);

const PAUSE_15_MINUTES: &str = "pause-15-minutes";
const PAUSE_1_HOUR: &str = "pause-1-hour";
const PAUSE_UNTIL_TOMORROW: &str = "pause-until-tomorrow";
const PAUSE_UNTIL_RESUMED: &str = "pause-until-resumed";
const RESUME: &str = "resume";
const OPEN: &str = "open";
const QUIT: &str = "quit";

pub fn create(app: &AppHandle) -> tauri::Result<()> {
    let state = app.state::<Pause>().state();
    TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon(state))
        .icon_as_template(true)
        .tooltip(tooltip(state))
        .menu(&menu(app, state)?)
        .show_menu_on_left_click(true)
        .on_menu_event(on_menu_event)
        .build(app)?;

    let app = app.clone();
    thread::spawn(move || watch_for_resume(app));
    Ok(())
}

fn on_menu_event(app: &AppHandle, event: MenuEvent) {
    let pause = app.state::<Pause>();
    match event.id().as_ref() {
        PAUSE_15_MINUTES => pause.pause(PauseLength::FifteenMinutes),
        PAUSE_1_HOUR => pause.pause(PauseLength::OneHour),
        PAUSE_UNTIL_TOMORROW => pause.pause(PauseLength::UntilTomorrow),
        PAUSE_UNTIL_RESUMED => pause.pause(PauseLength::UntilResumed),
        RESUME => pause.resume(),
        OPEN => {
            open_window(app);
            return;
        }
        QUIT => {
            app.exit(0);
            return;
        }
        _ => return,
    }
    save_pause(app);
    refresh(app);
}

/// Saves the pause state so it still holds after Lax restarts. Returns
/// whether it was saved. A failed save is retried by [`watch_for_resume`].
fn save_pause(app: &AppHandle) -> bool {
    let state = app.state::<Pause>().state();
    let Some(store) = app.try_state::<Mutex<Store>>() else {
        return false;
    };
    let store = store.lock().unwrap_or_else(PoisonError::into_inner);
    store.set_pause_state(state).is_ok()
}

/// Updates the icon, tooltip and menu to match the pause state.
fn refresh(app: &AppHandle) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return;
    };
    let state = app.state::<Pause>().state();
    // Failures here only leave the menu bar out of date. The pause state
    // itself, which capture checks, has already changed.
    let _ = tray.set_icon_with_as_template(Some(icon(state)), true);
    let _ = tray.set_tooltip(Some(tooltip(state)));
    if let Ok(menu) = menu(app, state) {
        let _ = tray.set_menu(Some(menu));
    }
}

/// Keeps the menu bar current when a timed pause runs out, or when its label
/// changes (for example "until 00:30 tomorrow" becoming "until 00:30").
///
/// It also keeps the saved pause state in step: a save that failed is retried
/// here, and a timed pause that ran out is cleared from storage.
fn watch_for_resume(app: AppHandle) {
    let mut shown = label(app.state::<Pause>().state());
    let mut saved = None;
    loop {
        thread::sleep(REFRESH_INTERVAL);
        let state = app.state::<Pause>().state();
        if saved != Some(state) && save_pause(&app) {
            saved = Some(state);
        }
        let current = label(state);
        if current != shown {
            refresh(&app);
            shown = current;
        }
    }
}

fn menu(app: &AppHandle, state: CaptureState) -> tauri::Result<Menu<Wry>> {
    let status = MenuItem::with_id(app, "status", label(state), false, None::<&str>)?;
    let item = |id, text| MenuItem::with_id(app, id, text, true, None::<&str>);
    let pause_15 = item(PAUSE_15_MINUTES, "Pause for 15 minutes")?;
    let pause_hour = item(PAUSE_1_HOUR, "Pause for 1 hour")?;
    let pause_tomorrow = item(PAUSE_UNTIL_TOMORROW, "Pause until tomorrow")?;
    let pause_resumed = item(PAUSE_UNTIL_RESUMED, "Pause until resumed")?;
    let open = item(OPEN, "Open Lax")?;
    let quit = item(QUIT, "Quit Lax")?;
    let separator = || PredefinedMenuItem::separator(app);

    let menu = Menu::with_items(app, &[&status])?;
    if state.is_paused() {
        menu.append(&item(RESUME, "Resume capture")?)?;
    }
    menu.append_items(&[
        &separator()?,
        &pause_15,
        &pause_hour,
        &pause_tomorrow,
        &pause_resumed,
        &separator()?,
        &open,
        &quit,
    ])?;
    Ok(menu)
}

fn icon(state: CaptureState) -> Image<'static> {
    if state.is_paused() {
        ICON_PAUSED
    } else {
        ICON_RUNNING
    }
}

fn label(state: CaptureState) -> String {
    state.label(&Local::now())
}

fn tooltip(state: CaptureState) -> String {
    format!("Lax: {}", label(state))
}

pub fn open_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}
