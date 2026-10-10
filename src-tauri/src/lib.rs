mod accessibility;
#[allow(dead_code)] // Used by capture, which arrives in #6.
mod exclusions;
mod expiry;
mod filevault;
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
        .invoke_handler(tauri::generate_handler![
            accessibility::accessibility_status,
            accessibility::open_accessibility_settings,
            filevault::filevault_enabled,
        ])
        .setup(|app| {
            // Lax lives in the menu bar, without a Dock icon.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            // Logs live in the private data folder:
            // ~/Library/Application Support/io.github.fnyman-ctrl.lax/logs
            // If the log can't be opened, Lax runs without one.
            let _ = logging::init(&app.path().app_data_dir()?);
            logging::info("app.started", &[]);

            // All data lives in one private folder:
            // ~/Library/Application Support/io.github.fnyman-ctrl.lax
            //
            // A pause set before Lax was quit still holds. It is restored before
            // anything else starts, so nothing is read in between. If it can't
            // be read, capture stays paused: stopping by mistake is safer than
            // capturing by mistake.
            //
            // If the database can't be opened (damaged, from a newer Lax, disk
            // full), Lax still starts so the user can see what's wrong, but
            // nothing can be stored. Showing the error in the window comes with
            // the viewer (#10).
            let paused_until_resumed = pause::CaptureState::Paused { until: None };
            let pause = app.state::<pause::Pause>();
            match store::Store::open(&app.path().app_data_dir()?) {
                Ok(store) => {
                    pause.restore(store.pause_state().unwrap_or(paused_until_resumed));
                    app.manage(Mutex::new(store));
                    expiry::start(app.handle().clone());
                }
                Err(_) => pause.restore(paused_until_resumed),
            }
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
