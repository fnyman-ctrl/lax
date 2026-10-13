mod accessibility;
mod ask;
#[cfg(target_os = "macos")]
mod ax;
#[cfg_attr(not(target_os = "macos"), allow(dead_code))] // The loop is macOS only.
mod capture;
mod exclusion_settings;
mod exclusions;
mod expiry;
mod filevault;
mod forget;
mod glass;
mod insert;
mod journal;
#[allow(dead_code)] // Some levels and fields have no user yet.
mod logging;
mod memory;
mod model_settings;
#[allow(dead_code)] // Embeddings and more are used by the journal (Stage 3).
mod models;
mod outbox;
mod pages;
mod pause;
#[allow(dead_code)] // RemoteRedactor is used by remote providers (#16).
mod redact;
mod setup;
mod site;
#[allow(dead_code)] // Plain search and setting the capture interval await the settings window.
mod store;
mod summon;
mod tray;
mod vectors;
mod viewer;
mod write;

use std::sync::Mutex;

use tauri::{Manager, WindowEvent};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(pause::Pause::default())
        .manage(models::ModelTurn::default())
        .manage(models::ModelHealth::default())
        .manage(capture::Heartbeat::default())
        .manage(exclusions::Current::default())
        .manage(summon::Summoned::default())
        .invoke_handler(tauri::generate_handler![
            accessibility::accessibility_status,
            accessibility::open_accessibility_settings,
            filevault::filevault_enabled,
            viewer::capture_status,
            viewer::recent_captures,
            viewer::search_captures,
            viewer::capture_detail,
            forget::captured_apps,
            forget::forget_captures,
            forget::delete_all_data,
            forget::retention_days,
            forget::set_retention_days,
            exclusion_settings::exclusion_settings,
            exclusion_settings::add_exclusion,
            exclusion_settings::remove_exclusion,
            exclusion_settings::set_default_excluded,
            model_settings::find_local_models,
            model_settings::model_settings,
            model_settings::set_model,
            model_settings::clear_model,
            model_settings::test_model,
            outbox::outbox_entries,
            outbox::outbox_payload,
            outbox::delete_outbox,
            outbox::outbox_retention_days,
            outbox::set_outbox_retention_days,
            journal::journal_status,
            journal::set_journal_enabled,
            journal::journal_episodes,
            ask::ask_memory,
            memory::memory_status,
            memory::set_memory_enabled,
            memory::memory_entities,
            memory::forget_entity,
            memory::forget_memory,
            memory::recreate_page,
            memory::recreate_about_me,
            memory::page_history,
            memory::restore_page_version,
            memory::set_own_names,
            memory::open_memory_folder,
            memory::keep_entity,
            memory::rename_entity,
            memory::merge_entities,
            memory::dont_learn_list,
            memory::allow_learning,
            setup::setup_status,
            setup::finish_setup,
            summon::writing_status,
            summon::set_writing_enabled,
            summon::set_writing_key,
            summon::close_panel,
            write::panel_call,
            write::write_draft,
            write::fit_panel,
            insert::insert_draft,
        ])
        .setup(|app| {
            // Lax lives in the menu bar, without a Dock icon.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            // Logs live in the private data folder:
            // ~/Library/Application Support/io.github.fnyman-ctrl.lax/logs
            // If the log can't be opened, Lax runs without one.
            let _ = logging::init(&app.path().app_data_dir()?);
            logging::log_panics();
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
            // full), Lax still starts so the user can see what's wrong in the
            // window, but nothing can be stored.
            let paused_until_resumed = pause::CaptureState::Paused { until: None };
            let pause = app.state::<pause::Pause>();
            match store::Store::open(&app.path().app_data_dir()?) {
                Ok(store) => {
                    pause.restore(store.pause_state().unwrap_or(paused_until_resumed));
                    // Capture only starts with the user's exclusions in
                    // place. If they can't be read, it doesn't start at all:
                    // capturing an app the user excluded is the worse mistake.
                    let exclusions = store.exclusions();
                    app.manage(Mutex::new(store));
                    expiry::start(app.handle().clone());
                    // Does nothing until the user turns the journal on. The
                    // memory runs in the same loop, after it.
                    journal::start(app.handle().clone());
                    match exclusions {
                        Ok(exclusions) => {
                            app.state::<exclusions::Current>().set(exclusions);
                            // Capture checks the pause and the Accessibility
                            // permission before every read, so it can start now.
                            #[cfg(target_os = "macos")]
                            capture::start(app.handle().clone());
                        }
                        Err(_) => {
                            app.manage(viewer::StoreUnavailable(
                                "your exclusions couldn't be read".to_string(),
                            ));
                        }
                    }
                }
                Err(error) => {
                    pause.restore(paused_until_resumed);
                    app.manage(viewer::StoreUnavailable(format!(
                        "the database couldn't be opened ({error})"
                    )));
                }
            }
            tray::create(app.handle())?;
            // Listens for ⌥ Option only if the user turned the writing
            // assistant on.
            summon::start(app.handle());

            // Without the Accessibility permission Lax can't capture anything,
            // so show the onboarding window explaining why it's needed.
            let granted = accessibility::is_granted();
            if !granted {
                tray::open_window(app.handle());
            }
            accessibility::watch(app.handle().clone(), granted, |app, granted| {
                if granted {
                    summon::start(app);
                } else {
                    summon::stop(app);
                    tray::open_window(app);
                }
            });
            Ok(())
        })
        .on_window_event(|window, event| match event {
            // Closing the window only hides it. Lax keeps running in the menu
            // bar until the user quits from there.
            WindowEvent::CloseRequested { api, .. } => {
                api.prevent_close();
                let _ = window.hide();
            }
            // The panel goes away when the user clicks elsewhere.
            WindowEvent::Focused(false) if window.label() == summon::PANEL => {
                let _ = window.hide();
                window.app_handle().state::<summon::Summoned>().forget();
            }
            _ => {}
        })
        .run(tauri::generate_context!())
        .expect("error while running Lax");
}

#[cfg(test)]
mod tests {
    /// Every command the window can call must be listed in `build.rs` and
    /// allowed in `capabilities/default.json`; Tauri refuses any other
    /// ("not allowed. Command not found"), and nothing else catches it. And
    /// nothing is listed or allowed that isn't a command any more.
    #[test]
    fn every_command_is_listed_and_allowed() {
        let lib = include_str!("lib.rs");
        let build = include_str!("../build.rs");
        let capabilities = [
            include_str!("../capabilities/default.json"),
            include_str!("../capabilities/panel.json"),
        ]
        .concat();
        let start =
            lib.find("generate_handler![").expect("the handler") + "generate_handler![".len();
        let end = start + lib[start..].find("])").expect("its end");
        let commands: Vec<&str> = lib[start..end]
            .split(',')
            .filter_map(|item| item.trim().rsplit("::").next())
            .filter(|name| {
                !name.is_empty() && name.chars().all(|c| c.is_ascii_lowercase() || c == '_')
            })
            .collect();
        assert!(commands.len() > 40, "found {} commands", commands.len());
        for command in &commands {
            assert!(
                build.contains(&format!("\"{command}\"")),
                "{command} is not listed in build.rs"
            );
            let permission = format!("\"allow-{}\"", command.replace('_', "-"));
            assert!(
                capabilities.contains(&permission),
                "{command} is not allowed in capabilities/"
            );
        }
        // The other way: every allowed permission and listed name is a command.
        for allowed in capabilities
            .split('"')
            .filter_map(|word| word.strip_prefix("allow-"))
        {
            assert!(
                commands.contains(&allowed.replace('-', "_").as_str()),
                "allow-{allowed} is granted but isn't a command"
            );
        }
        let listed = &build[build.find(".commands(&[").expect("the list")..];
        let listed = &listed[..listed.find("])").expect("its end")];
        for name in listed.split('"').skip(1).step_by(2) {
            assert!(
                commands.contains(&name),
                "{name} is listed in build.rs but isn't a command"
            );
        }
    }
}
