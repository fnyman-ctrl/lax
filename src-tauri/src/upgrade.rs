//! Upgrades keep the user's data (#170): every released version's database,
//! in `fixtures/<version>/`, opens with the current code, and everything in
//! it is still there. Test-only.

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use crate::keychain::fake::FakeKeychain;
    use crate::store::{ModelTask, Opened, Store};

    /// The fixtures' test key, never a real one: "7e57" ("test") sixteen times.
    const TEST_KEY: &[u8] = b"7e577e577e577e577e577e577e577e577e577e577e577e577e577e577e577e57";

    /// A release's database, named so `.gitignore`'s rule against committing
    /// databases (`*.sqlite`) stays as it is.
    const FIXTURE_FILE: &str = "database.fixture";

    /// The released versions with a fixture. Each release adds its own
    /// (docs/RELEASING.md).
    const RELEASES: &[&str] = &["0.1.0"];

    /// A copy of a release's database in a temporary folder, so the
    /// migrations never touch the fixture.
    struct Copy(PathBuf);

    impl Copy {
        fn of(release: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("lax-upgrade-{release}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("fixtures")
                .join(release)
                .join(FIXTURE_FILE);
            std::fs::copy(&fixture, dir.join(crate::store::DATABASE_FILE))
                .unwrap_or_else(|_| panic!("the fixture for {release}"));
            Copy(dir)
        }
    }

    impl Drop for Copy {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn every_released_database_opens_with_everything_in_it() {
        for release in RELEASES {
            let copy = Copy::of(release);
            let (store, opened) = Store::open_keyed(&copy.0, &FakeKeychain::holding(TEST_KEY))
                .unwrap_or_else(|e| panic!("{release} opens: {e:?}"));
            assert_eq!(opened, Opened::Encrypted, "{release}");
            check(&store, release);
            drop(store);
            // And again, after its migrations ran.
            let (store, _) = Store::open_keyed(&copy.0, &FakeKeychain::holding(TEST_KEY)).unwrap();
            check(&store, release);
        }
    }

    /// What each fixture holds (see the release's fixture maker).
    fn check(store: &Store, release: &str) {
        // The journal.
        let entries: Vec<String> = store
            .episodes(None, 10)
            .unwrap()
            .into_iter()
            .map(|e| e.summary)
            .collect();
        assert_eq!(entries.len(), 3, "{release}");
        assert!(
            entries.iter().any(|s| s.contains("Planerade Q3 lansering")),
            "{release}"
        );
        // The memory: names, kinds and days.
        let entities = store.entities().unwrap();
        let anna = entities
            .iter()
            .find(|e| e.name == "Anna Example")
            .unwrap_or_else(|| panic!("{release}: Anna Example"));
        assert_eq!((anna.mentions, anna.days), (2, 2), "{release}");
        assert!(!anna.is_new(), "{release}");
        assert!(
            entities.iter().any(|e| e.name == "Exempelbolaget AB"),
            "{release}"
        );
        assert!(
            entities.iter().any(|e| e.name == "Q3 lansering"),
            "{release}"
        );
        // Her page, with Lax's fact and the user's own line, and its history.
        let page = store.page_of(anna.id).unwrap().expect("Anna's page");
        let text = store.page_text(&page.id).unwrap().expect("the page's text");
        assert!(
            text.contains("Min egen anteckning om Anna."),
            "{release}: {text}"
        );
        assert_eq!(
            page.facts.as_deref(),
            Some(&["Leder Q3-lanseringen.".to_string()][..])
        );
        assert!(
            store.page_history(&page.id, 10).unwrap().len() >= 2,
            "{release}"
        );
        assert!(store.about_me_page().unwrap().is_some(), "{release}");
        // Settings.
        assert!(store.journal_enabled().unwrap(), "{release}");
        assert!(store.memory_enabled().unwrap(), "{release}");
        assert!(store.writing_enabled().unwrap(), "{release}");
        // The pages are rows (ADR 0011): the user's line above is read from
        // the row, as every page is since #169.
        assert_eq!(store.own_names().unwrap(), ["Bo Exempel"], "{release}");
        let chat = store
            .model_choice(ModelTask::Chat)
            .unwrap()
            .expect("a chat model");
        assert_eq!(chat.model, "example-chat-model", "{release}");
    }
}
