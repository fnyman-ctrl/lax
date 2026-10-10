mod pause;
mod tray;

use tauri::WindowEvent;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(pause::Pause::default())
        .setup(|app| {
            // Lax lives in the menu bar, without a Dock icon.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            tray::create(app.handle())?;
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
