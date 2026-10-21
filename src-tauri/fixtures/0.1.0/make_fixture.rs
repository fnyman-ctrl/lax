//! Makes the upgrade fixture with this version's code. Not committed here:
//! run once in a worktree of the release tag, then copied to main.
#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::path::PathBuf;

    use chrono::{FixedOffset, TimeZone, Utc};

    use crate::keychain::fake::FakeKeychain;
    use crate::store::{EntityKind, ModelChoice, ModelTask, NewCapture, NewEpisode, Store};

    /// A test key, never a real one: "7e57" ("test") sixteen times.
    const KEY: &[u8] = b"7e577e577e577e577e577e577e577e577e577e577e577e577e577e577e577e57";

    #[test]
    #[ignore = "makes the upgrade fixture: FIXTURE_OUT=<dir>"]
    fn make_upgrade_fixture() {
        let out = PathBuf::from(std::env::var("FIXTURE_OUT").expect("FIXTURE_OUT"));
        std::fs::create_dir_all(&out).unwrap();
        let tz = FixedOffset::east_opt(2 * 3600).unwrap();
        let at = |day: u32, hour: u32| tz.with_ymd_and_hms(2026, 10, day, hour, 0, 0).unwrap().with_timezone(&Utc);
        {
            let (mut store, _) = Store::open_keyed(&out, &FakeKeychain::holding(KEY)).unwrap();
            let entries = [
                (at(9, 10), "Mail", "Mailade Anna Example om offerten till Exempelbolaget AB.",
                 vec![(EntityKind::Person, "Anna Example"), (EntityKind::Organisation, "Exempelbolaget AB")]),
                (at(10, 14), "Slack", "Planerade Q3 lansering med Anna Example.",
                 vec![(EntityKind::Person, "Anna Example"), (EntityKind::Project, "Q3 lansering")]),
                (at(10, 15), "Safari", "Läste om Exempelbolaget AB på example.com.",
                 vec![(EntityKind::Organisation, "Exempelbolaget AB")]),
            ];
            for (when, app, summary, names) in entries {
                let capture = store
                    .insert(&NewCapture {
                        captured_at: when,
                        bundle_id: &format!("com.example.{}", app.to_lowercase()),
                        app_name: app,
                        window_title: "Synthetic window",
                        site: None,
                        text: summary,
                    })
                    .unwrap()
                    .unwrap();
                let episode = store
                    .record_episode(
                        &NewEpisode { summary: summary.to_string(), model: "example-model".to_string(), created_at: when },
                        &[capture],
                    )
                    .unwrap()
                    .unwrap();
                let names: Vec<_> = names.into_iter().map(|(k, n)| (k, n.to_string())).collect();
                store.record_mentions(episode, &names, when).unwrap();
            }
            store.set_journal_enabled(true).unwrap();
            store.set_memory_enabled(true).unwrap();
            store.set_writing_enabled(true).unwrap();
            store.set_own_names(&["Bo Exempel".to_string()]).unwrap();
            store
                .set_model_choice(
                    ModelTask::Chat,
                    Some(&ModelChoice { base_url: "http://localhost:1234/v1".to_string(), model: "example-chat-model".to_string() }),
                )
                .unwrap();
            store.set_pages_in_database().unwrap();
            crate::pages::sync_rows(&mut store, &HashSet::new(), at(10, 16), &tz).unwrap();
            let anna = store.entities().unwrap().into_iter().find(|e| e.name == "Anna Example").unwrap().id;
            let page = store.page_of(anna).unwrap().unwrap();
            store.set_facts(&page.id, &["Leder Q3-lanseringen.".to_string()], &[], at(10, 17)).unwrap();
            crate::pages::sync_rows(&mut store, &HashSet::new(), at(10, 17), &tz).unwrap();
            let text = store.page_text(&page.id).unwrap().unwrap();
            let mine = format!("{text}\nMin egen anteckning om Anna.\n");
            assert!(store.save_page_text(&page.id, &text, &mine, true, at(10, 18)).unwrap());
        }
        assert!(!out.join("lax.sqlite-wal").exists() || std::fs::metadata(out.join("lax.sqlite-wal")).unwrap().len() == 0);
    }
}
