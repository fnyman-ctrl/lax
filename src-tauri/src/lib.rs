mod accessibility;
mod ask;
#[cfg(target_os = "macos")]
mod ax;
#[cfg_attr(not(target_os = "macos"), allow(dead_code))] // The loop is macOS only.
mod capture;
mod database;
mod exclusion_settings;
mod exclusions;
mod expiry;
mod filevault;
mod forget;
mod glass;
mod insert;
mod journal;
#[cfg(target_os = "macos")]
mod keychain;
#[allow(dead_code)] // The panel's unlock comes next (ADR 0012, step 4).
mod lock;
#[allow(dead_code)] // Some levels and fields have no user yet.
mod logging;
mod memory;
mod merges;
mod model_settings;
#[allow(dead_code)] // Embeddings and more are used by the journal (Stage 3).
mod models;
mod outbox;
mod pages;
mod pause;
#[allow(dead_code)] // RemoteRedactor is used by remote providers (#16).
mod redact;
mod report;
mod setup;
mod site;
#[allow(dead_code)] // Plain search and setting the capture interval await the settings window.
mod store;
mod summon;
mod tray;
mod upgrade;
mod vectors;
mod viewer;
mod write;

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
        .manage(database::Database::default())
        .manage(pages::OpenPages::default())
        .manage(pages::export::ChosenFolder::default())
        // Lax starts locked (ADR 0012).
        .manage(lock::app::new())
        // Every command passes the lock's gate first. One without a rule in
        // lock.rs is refused (`every_command_has_a_lock_rule`).
        .invoke_handler(lock::app::gate(tauri::generate_handler![
            lock::app::lock_status,
            lock::app::unlock,
            accessibility::accessibility_status,
            report::bug_report_details,
            accessibility::open_accessibility_settings,
            filevault::filevault_enabled,
            database::database_status,
            database::continue_opening,
            database::retry_opening,
            database::start_over,
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
            memory::page_text,
            memory::save_page,
            memory::close_page,
            memory::pick_export_folder,
            memory::export_pages,
            memory::page_notes,
            memory::keep_entity,
            memory::rename_entity,
            memory::merge_entities,
            memory::obvious_pairs,
            memory::not_same,
            memory::merge_obvious_pairs,
            memory::set_part_of,
            memory::take_out_part,
            merges::merge_suggestions,
            merges::likely_same,
            ask::brief_on,
            merges::part_suggestions,
            merges::not_part,
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
        ]))
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
            // Nothing is captured until the database is open and the pause
            // saved in it is restored: stopping by mistake is safer than
            // capturing by mistake. The database opens on a thread of its
            // own, since reading its key can make macOS ask for the login
            // password; the window says where it stands (ADR 0010). Capture,
            // expiry, the journal and the writing assistant start once it is
            // open.
            app.state::<pause::Pause>()
                .restore(pause::CaptureState::Paused { until: None });
            tray::create(app.handle())?;
            database::start(app.handle());
            // Locks when the user is away, whether capture runs or not.
            lock::app::watch(app.handle());

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
                if window.label() == lock::MAIN_WINDOW {
                    lock::app::hidden(window.app_handle());
                }
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
    use crate::lock::Action;

    /// The commands in `generate_handler!`, each with its module path, such
    /// as `("forget", "forget_captures")`.
    fn handler() -> Vec<(&'static str, &'static str)> {
        let lib = include_str!("lib.rs");
        let start =
            lib.find("generate_handler![").expect("the handler") + "generate_handler![".len();
        let end = start + lib[start..].find("])").expect("its end");
        let commands: Vec<(&str, &str)> = lib[start..end]
            .split(',')
            .filter_map(|item| item.trim().rsplit_once("::"))
            .filter(|(_, name)| {
                !name.is_empty() && name.chars().all(|c| c.is_ascii_lowercase() || c == '_')
            })
            .collect();
        assert!(commands.len() > 40, "found {} commands", commands.len());
        commands
    }

    /// The commands in `generate_handler!`, by name.
    fn commands() -> Vec<&'static str> {
        handler().into_iter().map(|(_, name)| name).collect()
    }

    /// The source of the module a command is in, for the commands that ask.
    fn source(module: &str) -> Option<&'static str> {
        Some(match module {
            "database" => include_str!("database.rs"),
            "exclusion_settings" => include_str!("exclusion_settings.rs"),
            "forget" => include_str!("forget.rs"),
            "journal" => include_str!("journal.rs"),
            "memory" => include_str!("memory.rs"),
            "model_settings" => include_str!("model_settings.rs"),
            "outbox" => include_str!("outbox.rs"),
            "summon" => include_str!("summon.rs"),
            _ => return None,
        })
    }

    /// The body of `command` in `module`'s source.
    fn body(module: &str, command: &str) -> &'static str {
        let source =
            source(module).unwrap_or_else(|| panic!("add {module}.rs to `source` for {command}"));
        let start = source
            .find(&format!("pub async fn {command}("))
            .unwrap_or_else(|| panic!("{command} isn't a `pub async fn` in {module}.rs"));
        let end = start + source[start..].find("\n}\n").expect("its end");
        &source[start..end]
    }

    /// The permissions a capability file grants, as command names.
    fn allowed(capability: &str) -> Vec<String> {
        capability
            .split('"')
            .filter_map(|word| word.strip_prefix("allow-"))
            .map(|allowed| allowed.replace('-', "_"))
            .collect()
    }

    /// Every command the window can call must be listed in `build.rs` and
    /// allowed in `capabilities/default.json`; Tauri refuses any other
    /// ("not allowed. Command not found"), and nothing else catches it. And
    /// nothing is listed or allowed that isn't a command any more.
    #[test]
    fn every_command_is_listed_and_allowed() {
        let build = include_str!("../build.rs");
        let capabilities = [
            include_str!("../capabilities/default.json"),
            include_str!("../capabilities/panel.json"),
        ]
        .concat();
        let commands = commands();
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
        for allowed in allowed(&capabilities) {
            assert!(
                commands.contains(&allowed.as_str()),
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

    /// Every command has exactly one lock rule in `lock.rs` (ADR 0012), so
    /// the gate knows it: a command without one is refused. The rules match
    /// the capabilities: the panel's commands are the panel's, and the main
    /// window's are the main window's.
    #[test]
    fn every_command_has_a_lock_rule() {
        use crate::lock::{ASKS_WHILE_LOCKED, EVERY_TIME, LOOSENING, OPEN, PANEL, UNLOCKED, rule};

        let names = |list: &[(&'static str, Action)]| -> Vec<&'static str> {
            list.iter().map(|(command, _)| *command).collect()
        };
        let main_lists = [
            OPEN.to_vec(),
            UNLOCKED.to_vec(),
            names(EVERY_TIME),
            names(LOOSENING),
            names(ASKS_WHILE_LOCKED),
        ];
        let lists: Vec<&Vec<&str>> = main_lists.iter().collect();
        let panel_list = PANEL.to_vec();
        let all: Vec<&Vec<&str>> = lists.iter().copied().chain([&panel_list]).collect();

        let commands = commands();
        for command in &commands {
            let found = all.iter().filter(|list| list.contains(command)).count();
            assert_eq!(found, 1, "{command} is in {found} of the lock's lists");
            assert!(rule(command).is_some(), "{command}");
        }
        for command in all.iter().flat_map(|list| list.iter()) {
            assert!(
                commands.contains(command),
                "{command} has a lock rule but isn't a command"
            );
            let times = all
                .iter()
                .flat_map(|list| list.iter())
                .filter(|c| *c == command);
            assert_eq!(times.count(), 1, "{command} is listed twice");
        }

        let mut main = allowed(include_str!("../capabilities/default.json"));
        let mut panel = allowed(include_str!("../capabilities/panel.json"));
        let mut main_rules: Vec<String> = lists
            .iter()
            .flat_map(|list| list.iter())
            .map(|c| c.to_string())
            .collect();
        let mut panel_rules: Vec<String> = PANEL.iter().map(|c| c.to_string()).collect();
        for list in [&mut main, &mut panel, &mut main_rules, &mut panel_rules] {
            list.sort();
        }
        assert_eq!(main, main_rules, "the main window's commands");
        assert_eq!(panel, panel_rules, "the panel's commands");
    }

    /// Every command that asks again (ADR 0012, "What else asks") asks with
    /// its own action, through the lock, before it changes anything: its
    /// body confirms with the action its list names, and runs off the main
    /// thread (`asking`). And a command whose name says it forgets, deletes,
    /// exports or loosens can't be left out of the lists: it would change
    /// things without asking.
    #[test]
    fn every_command_that_asks_asks_with_its_action() {
        use crate::lock::{ASKS_WHILE_LOCKED, EVERY_TIME, LOOSENING, action};

        let handler = handler();
        let module = |command: &str| {
            handler
                .iter()
                .find(|(_, name)| *name == command)
                .map(|(module, _)| *module)
                .unwrap_or_else(|| panic!("{command} isn't a command"))
        };
        let mut actions = Vec::new();
        for (list, confirms) in [
            (EVERY_TIME, "confirmed"),
            (LOOSENING, "confirmed_if_loosening"),
            (ASKS_WHILE_LOCKED, "confirmed_while_locked"),
        ] {
            for (command, wanted) in list {
                let body = body(module(command), command);
                assert!(
                    body.contains("asking("),
                    "{command} doesn't ask off the main thread"
                );
                let call = format!(".{confirms}(Action::{wanted:?}");
                assert!(body.contains(&call), "{command} doesn't call {call}");
                // No other action.
                assert_eq!(body.matches("Action::").count(), 1, "{command}");
                assert!(!actions.contains(wanted), "{wanted:?} is used twice");
                actions.push(*wanted);
            }
        }

        // What the names promise.
        let must_ask = |name: &str| {
            [
                "forget_",
                "delete_",
                "export_",
                "remove_",
                "allow_",
                "start_over",
            ]
            .iter()
            .any(|start| name.starts_with(start))
                || name.ends_with("_retention_days") && name.starts_with("set_")
                || name == "set_own_names"
        };
        for (_, command) in &handler {
            if must_ask(command) {
                assert!(
                    EVERY_TIME
                        .iter()
                        .chain(ASKS_WHILE_LOCKED)
                        .any(|(c, _)| c == command),
                    "{command} must ask every time"
                );
            }
            if command.starts_with("set_") && command.ends_with("_enabled") {
                assert!(
                    LOOSENING.iter().any(|(c, _)| c == command),
                    "{command} must ask when it turns something on"
                );
            }
            // A command not in the lists doesn't ask: one that asks must be
            // listed, so the gate and these tests know it.
            if action(command).is_none()
                && let Some(source) = source(module(command))
                && let Some(start) = source.find(&format!("fn {command}("))
            {
                let rest = &source[start..];
                let body = &rest[..rest.find("\n}\n").unwrap_or(rest.len())];
                assert!(
                    !body.contains(".confirm"),
                    "{command} asks but isn't listed"
                );
            }
        }
    }
}
