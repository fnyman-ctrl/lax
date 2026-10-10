mod accessibility;
mod pause;
mod tray;

use tauri::WindowEvent;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(pause::Pause::default())
        .invoke_handler(tauri::generate_handler![
            accessibility::accessibility_status,
            accessibility::open_accessibility_settings,
        ])
        .setup(|app| {
            // Lax lives in the menu bar, without a Dock icon.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            tray::create(app.handle())?;

            // Without the Accessibility permission Lax can't capture anything,
            // so show the onboarding window explaining why it's needed.
            let granted = accessibility::is_granted();
            if !granted {
                tray::open_window(app.handle());
            }
            accessibility::watch(app.handle().clone(), granted, |app, granted| {
                if !granted {
                    tray::open_window(app);
                }
            });
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
