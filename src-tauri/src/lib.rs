#[allow(dead_code)] // Used by capture, which arrives in #6.
mod exclusions;
mod expiry;
#[allow(dead_code)] // Most levels and fields are first used by capture (#6).
mod logging;
mod pause;
#[allow(dead_code)] // Used by capture, which arrives in #6.
mod redact;
#[allow(dead_code)] // Used by capture, which arrives in #6.
mod site;
#[allow(dead_code)] // Writing and searching are used by capture (#6) and the viewer (#10).
mod store;
mod tray;

use std::sync::Mutex;

use tauri::{Manager, WindowEvent};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(pause::Pause::default())
        .setup(|app| {
            // Lax lives in the menu bar, without a Dock icon.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            tray::create(app.handle())?;

            // Logs live in the private data folder:
            // ~/Library/Application Support/io.github.fnyman-ctrl.lax/logs
            // If the log can't be opened, Lax runs without one.
            let _ = logging::init(&app.path().app_data_dir()?);
            logging::info("app.started", &[]);

            // All data lives in one private folder:
            // ~/Library/Application Support/io.github.fnyman-ctrl.lax
            // If the database can't be opened (damaged, from a newer Lax, disk
            // full), Lax still starts so the user can pause and see what's
            // wrong, but nothing can be stored. Showing the error in the
            // window comes with the viewer (#10).
            if let Ok(store) = store::Store::open(&app.path().app_data_dir()?) {
                app.manage(Mutex::new(store));
                expiry::start(app.handle().clone());
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the window only hides it. Lax keeps running in the menu
            // bar until the user quits from there.
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running Lax");
}
