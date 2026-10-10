mod expiry;
mod pause;
#[allow(dead_code)] // Used by capture, which arrives in #6.
mod redact;
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

            // All data lives in one private folder:
            // ~/Library/Application Support/io.github.fnyman-ctrl.lax
            let store = store::Store::open(&app.path().app_data_dir()?)?;
            app.manage(Mutex::new(store));
            expiry::start(app.handle().clone());
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
