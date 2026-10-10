#[allow(dead_code)] // Most levels and fields are first used by capture (#6).
mod logging;
mod pause;
#[allow(dead_code)] // Used by capture, which arrives in #6.
mod site;
mod tray;

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
