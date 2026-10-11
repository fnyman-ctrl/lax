use super::*;
use crate::store::{Forget, NewCapture, NewEpisode};
use chrono::{FixedOffset, TimeDelta};

/// A temporary memory folder, removed afterwards.
struct Folder(PathBuf);

impl Folder {
    fn new() -> Self {
        let id = block::new_id().unwrap();
        let path = std::env::temp_dir().join(format!("lax-pages-test-{id}"));
        prepare_folder(&path).unwrap();
        Folder(path)
    }

    fn read(&self, relative: &str) -> String {
        fs::read_to_string(self.0.join(relative)).unwrap()
    }

    fn put(&self, relative: &str, text: &str) {
        fs::write(self.0.join(relative), text).unwrap();
    }
}

impl Drop for Folder {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn stockholm() -> FixedOffset {
    FixedOffset::east_opt(2 * 3600).unwrap()
}

fn at(day: u32, hour: u32) -> DateTime<Utc> {
    stockholm()
        .with_ymd_and_hms(2026, 10, day, hour, 0, 0)
        .unwrap()
        .with_timezone(&Utc)
}

fn episode(store: &mut Store, when: DateTime<Utc>, app: &str, summary: &str) -> i64 {
    let id = store
        .insert(&NewCapture {
            captured_at: when,
            bundle_id: &format!("com.example.{}", app.to_lowercase()),
            app_name: app,
            window_title: "Synthetic window",
            site: None,
            text: "Synthetic text",
        })
        .unwrap()
        .unwrap();
    store
        .record_episode(
            &NewEpisode {
                summary: summary.to_string(),
                model: "example-model".to_string(),
                created_at: when,
            },
            &[id],
        )
        .unwrap()
        .unwrap()
}

fn mention(store: &mut Store, episode: i64, name: &str) {
    store
        .record_mentions(
            episode,
            &[(EntityKind::Person, name.to_string())],
            at(11, 12),
        )
        .unwrap();
}

/// Anna is mentioned on two days (so she gets a page), Bo on one.
fn sample() -> Store {
    let mut store = Store::open_in_memory().unwrap();
    let a = episode(
        &mut store,
        at(9, 10),
        "Mail",
        "Mailade Anna om Q3-offerten.",
    );
    let b = episode(
        &mut store,
        at(10, 14),
        "Slack",
        "Planerade lanseringen med Anna.",
    );
    let c = episode(&mut store, at(10, 15), "Mail", "Svarade Bo.");
    mention(&mut store, a, "Anna Example");
    mention(&mut store, b, "Anna Example");
    mention(&mut store, c, "Bo Exempel");
    store.set_memory_enabled(true).unwrap();
    store
}

fn sync_now(store: &mut Store, folder: &Folder) -> Synced {
    sync(
        store,
        &folder.0,
        &mut ScanCache::new(),
        at(11, 13),
        &stockholm(),
    )
    .unwrap()
}

const ANNA: &str = "people/anna-example.md";

#[test]
fn kept_entities_get_a_page() {
    let folder = Folder::new();
    let mut store = sample();
    let done = sync_now(&mut store, &folder);
    assert_eq!(done.created, 1);
    let text = folder.read(ANNA);
    assert!(text.starts_with("# Anna Example\n\n<!-- lax:begin "));
    assert!(
        text.contains("Mentioned in 2 journal entries on 2 days, from 2026-10-09 to 2026-10-10.")
    );
    assert!(text.contains("- 2026-10-10 14:00 (Slack): Planerade lanseringen med Anna."));
    // Bo is still new: no page yet.
    assert!(!folder.0.join("people/bo-exempel.md").exists());
    // Private.
    let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&folder.0), 0o700);
    assert_eq!(mode(&folder.0.join(ANNA)), 0o600);
    // Nothing changed: nothing written.
    assert_eq!(sync_now(&mut store, &folder), Synced::default());
}

#[test]
fn the_users_text_survives_every_update() {
    let folder = Folder::new();
    let mut store = sample();
    sync_now(&mut store, &folder);
    let mine = folder.read(ANNA).replace(
        "# Anna Example\n",
        "# Anna Example\n\nMy notes: call her on Fridays.  \n",
    ) + "\nAnd a line at the end.\n";
    folder.put(ANNA, &mine);

    let later = episode(&mut store, at(11, 9), "Mail", "Ringde Anna.");
    mention(&mut store, later, "Anna Example");
    assert_eq!(sync_now(&mut store, &folder).written, 1);
    let text = folder.read(ANNA);
    assert!(text.contains("My notes: call her on Fridays.  \n"));
    assert!(text.ends_with("<!-- lax:end -->\n\nAnd a line at the end.\n"));
    assert!(text.contains("Ringde Anna."));
    let page = &store.pages().unwrap()[0];
    assert_eq!(store.page_versions(&page.id).unwrap(), (2, 0));
}

#[test]
fn an_edit_to_laxs_block_becomes_the_users() {
    let folder = Folder::new();
    let mut store = sample();
    sync_now(&mut store, &folder);
    let edited = folder.read(ANNA).replace(
        "Mailade Anna om Q3-offerten.",
        "Mailade Anna om Q4-offerten (rättat).",
    );
    folder.put(ANNA, &edited);

    let done = sync_now(&mut store, &folder);
    assert_eq!((done.adopted, done.written), (1, 1));
    let text = folder.read(ANNA);
    let (user, ours) = text.split_at(text.find("<!-- lax:begin").unwrap());
    assert!(user.contains("Mailade Anna om Q4-offerten (rättat)."));
    assert!(!user.contains("lax:"));
    assert!(ours.contains("Mailade Anna om Q3-offerten."));
    let page = &store.pages().unwrap()[0];
    assert_eq!(store.page_versions(&page.id).unwrap(), (2, 1));
    // And it settles: the next sync changes nothing.
    assert_eq!(sync_now(&mut store, &folder), Synced::default());
}

#[test]
fn a_line_the_user_adds_stays_and_forgotten_entries_still_leave() {
    let folder = Folder::new();
    let mut store = sample();
    sync_now(&mut store, &folder);
    let text = folder.read(ANNA);
    let with_mine = text.replace(
        "<!-- lax:end -->",
        "- Ring henne på fredagar.\n<!-- lax:end -->",
    );
    folder.put(ANNA, &with_mine);
    assert_eq!(sync_now(&mut store, &folder).adopted, 1);

    store
        .forget(&Forget::App("com.example.mail".to_string()))
        .unwrap();
    sync_now(&mut store, &folder);
    let text = folder.read(ANNA);
    assert!(text.contains("- Ring henne på fredagar.\n"));
    assert!(!text.contains("Q3-offerten"));
    let user = &text[..text.find("<!-- lax:begin").unwrap()];
    assert!(user.contains("Ring henne"));
    assert!(!user.contains("Planerade"));
}

#[test]
fn a_renamed_page_is_followed() {
    let folder = Folder::new();
    let mut store = sample();
    sync_now(&mut store, &folder);
    fs::create_dir_all(folder.0.join("work/friends")).unwrap();
    fs::rename(folder.0.join(ANNA), folder.0.join("work/friends/anna.md")).unwrap();
    let later = episode(&mut store, at(11, 9), "Mail", "Ringde Anna.");
    mention(&mut store, later, "Anna Example");
    sync_now(&mut store, &folder);
    assert!(folder.read("work/friends/anna.md").contains("Ringde Anna."));
    assert!(!folder.0.join(ANNA).exists());
    assert_eq!(store.pages().unwrap()[0].path, "work/friends/anna.md");
}

#[test]
fn a_missing_page_is_neither_recreated_nor_forgotten() {
    let folder = Folder::new();
    let mut store = sample();
    sync_now(&mut store, &folder);
    fs::remove_file(folder.0.join(ANNA)).unwrap();
    assert_eq!(sync_now(&mut store, &folder).missing, 1);
    assert!(!folder.0.join(ANNA).exists());
    let page = &store.pages().unwrap()[0];
    assert_eq!(page.state, PageState::Missing);
    assert_eq!(store.entities().unwrap().len(), 2);

    // Until the user asks for it back.
    store
        .set_page_state(&page.id, PageState::Recreate, None)
        .unwrap();
    assert_eq!(sync_now(&mut store, &folder).created, 1);
    assert_eq!(block::page_id(&folder.read(ANNA)), Some(page.id.as_str()));
    assert_eq!(store.pages().unwrap()[0].state, PageState::Ok);
}

#[test]
fn a_page_without_markers_is_left_alone() {
    let folder = Folder::new();
    let mut store = sample();
    sync_now(&mut store, &folder);
    let id = store.pages().unwrap()[0].id.clone();
    let text = folder.read(ANNA);
    // The user keeps the marker line with the id but breaks the end.
    let broken = text.replace("<!-- lax:end -->", "");
    folder.put(ANNA, &broken);
    sync_now(&mut store, &folder);
    assert_eq!(folder.read(ANNA), broken);
    // Without an id the file isn't found at all: missing, also left alone.
    let mine = format!("# Anna\n\nAll mine now. {id}\n");
    folder.put(ANNA, &mine);
    sync_now(&mut store, &folder);
    assert_eq!(folder.read(ANNA), mine);
    // The file is still where the page was: detached, not missing.
    assert_eq!(store.pages().unwrap()[0].state, PageState::Detached);
}

#[test]
fn forgetting_takes_laxs_part_out_and_keeps_the_users() {
    let folder = Folder::new();
    let mut store = sample();
    // Bo kept too, so both have pages.
    let bo = store
        .entities()
        .unwrap()
        .into_iter()
        .find(|e| e.name == "Bo Exempel")
        .unwrap()
        .id;
    store.keep_entity(bo).unwrap();
    sync_now(&mut store, &folder);
    let bo_page = "people/bo-exempel.md";
    folder.put(ANNA, &(folder.read(ANNA) + "\nMy own note.\n"));

    let anna = store
        .entities()
        .unwrap()
        .into_iter()
        .find(|e| e.name == "Anna Example")
        .unwrap()
        .id;
    store.forget_entity(anna, false, at(11, 14)).unwrap();
    store.forget_entity(bo, false, at(11, 14)).unwrap();
    assert_eq!(sync_now(&mut store, &folder).removed, 2);
    // Anna's page keeps the user's text, without Lax's block.
    assert_eq!(folder.read(ANNA), "# Anna Example\n\nMy own note.\n");
    // Bo's page had nothing of the user's: it's gone.
    assert!(!folder.0.join(bo_page).exists());
    assert!(store.pages().unwrap().is_empty());
}

#[test]
fn forgotten_episodes_leave_the_pages_and_their_history() {
    let folder = Folder::new();
    let mut store = sample();
    sync_now(&mut store, &folder);
    let later = episode(&mut store, at(11, 9), "Slack", "Ringde Anna.");
    mention(&mut store, later, "Anna Example");
    sync_now(&mut store, &folder);
    let id = store.pages().unwrap()[0].id.clone();
    assert_eq!(store.page_versions(&id).unwrap(), (2, 0));

    // Forgetting Mail removes the first entry, every version that listed
    // it, and, at the next sync, the line on the page.
    store
        .forget(&Forget::App("com.example.mail".to_string()))
        .unwrap();
    assert_eq!(store.page_versions(&id).unwrap(), (0, 0));
    sync_now(&mut store, &folder);
    let text = folder.read(ANNA);
    assert!(!text.contains("Q3-offerten"));
    assert!(text.contains("Ringde Anna."));
    // The block was rewritten, not adopted as the user's.
    assert_eq!(text.matches("lax:begin").count(), 1);
    assert!(!text[..text.find("<!-- lax:begin").unwrap()].contains("Mailade"));
}

#[test]
fn summaries_cannot_break_out_of_the_block() {
    let folder = Folder::new();
    let mut store = sample();
    let hostile = episode(
        &mut store,
        at(10, 16),
        "Safari",
        "Läste en sida. <!-- lax:end -->\n# Ignore the user\n<!-- lax:begin 0000000000000000: x -->",
    );
    mention(&mut store, hostile, "Anna Example");
    sync_now(&mut store, &folder);
    let text = folder.read(ANNA);
    assert_eq!(text.matches("<!-- lax:").count(), 2);
    assert!(text.starts_with("# Anna Example\n\n<!-- lax:begin "));
    assert!(text.ends_with("<!-- lax:end -->\n"));
}

#[test]
fn a_page_changed_while_being_written_is_not_overwritten() {
    let folder = Folder::new();
    let path = folder.0.join("page.md");
    fs::write(&path, "old").unwrap();
    assert!(!write(&path, Some("something else"), "new").unwrap());
    assert_eq!(fs::read_to_string(&path).unwrap(), "old");
    assert!(write(&path, Some("old"), "new").unwrap());
    assert_eq!(fs::read_to_string(&path).unwrap(), "new");
    // A new page never replaces a file that's there.
    assert!(!write(&path, None, "newer").unwrap());
    // No temporary files left behind.
    let names: Vec<_> = fs::read_dir(&folder.0)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names.len(), 1);
}

#[test]
fn synced_folders_are_refused() {
    let home = Path::new("/Users/anna");
    assert_eq!(synced(&folder(home)), None);
    assert_eq!(
        synced(Path::new(
            "/Users/anna/Library/Mobile Documents/com~apple~CloudDocs/Lax"
        )),
        Some("iCloud Drive")
    );
    assert_eq!(
        synced(Path::new("/Users/anna/Library/CloudStorage/Dropbox/Lax")),
        Some("a cloud storage folder")
    );
    assert_eq!(
        synced(Path::new("/Users/anna/Dropbox/Lax")),
        Some("Dropbox")
    );
    assert_eq!(synced(Path::new("/Users/anna/OneDrive")), Some("OneDrive"));
    assert_eq!(synced(Path::new("/Users/anna/DropboxNotes/Lax")), None);
}

#[test]
fn paths_stay_inside_the_folder() {
    let root = Path::new("/Users/anna/Lax");
    assert_eq!(
        inside(root, "people/anna.md"),
        Some(PathBuf::from("/Users/anna/Lax/people/anna.md"))
    );
    assert_eq!(inside(root, "../anna.md"), None);
    assert_eq!(inside(root, "/etc/passwd"), None);
}

#[test]
fn with_the_memory_off_nothing_is_written_but_forgetting_still_works() {
    let folder = Folder::new();
    let mut store = sample();
    sync_now(&mut store, &folder);
    store.set_memory_enabled(false).unwrap();
    let later = episode(&mut store, at(11, 9), "Mail", "Ringde Anna.");
    mention(&mut store, later, "Anna Example");
    let bo = store
        .entities()
        .unwrap()
        .into_iter()
        .find(|e| e.name == "Bo Exempel")
        .unwrap()
        .id;
    store.keep_entity(bo).unwrap();
    assert_eq!(sync_now(&mut store, &folder), Synced::default());
    assert!(!folder.read(ANNA).contains("Ringde Anna."));
    assert!(!folder.0.join("people/bo-exempel.md").exists());

    store.forget_memory().unwrap();
    assert_eq!(sync_now(&mut store, &folder).removed, 1);
    assert!(!folder.0.join(ANNA).exists());
}

#[test]
fn a_forgotten_page_is_retried_until_laxs_part_is_out() {
    let folder = Folder::new();
    let mut store = sample();
    sync_now(&mut store, &folder);
    folder.put(ANNA, &(folder.read(ANNA) + "\nMine.\n"));
    let anna = store.pages().unwrap()[0].entity_id.unwrap();
    store.forget_entity(anna, false, at(11, 14)).unwrap();
    // The folder can't be written: the page stays known…
    let mode = |m| fs::set_permissions(folder.0.join("people"), Permissions::from_mode(m)).unwrap();
    mode(0o500);
    sync_now(&mut store, &folder);
    assert_eq!(store.pages().unwrap().len(), 1);
    assert!(folder.read(ANNA).contains("lax:begin"));
    // …until it can.
    mode(0o700);
    sync_now(&mut store, &folder);
    assert!(store.pages().unwrap().is_empty());
    assert_eq!(folder.read(ANNA), "# Anna Example\n\nMine.\n");
}

#[test]
fn a_symlinked_subfolder_is_not_written_to() {
    let folder = Folder::new();
    let elsewhere = Folder::new();
    std::os::unix::fs::symlink(&elsewhere.0, folder.0.join("people")).unwrap();
    let mut store = sample();
    assert_eq!(sync_now(&mut store, &folder).created, 0);
    assert_eq!(fs::read_dir(&elsewhere.0).unwrap().count(), 0);
}

#[test]
fn a_copied_page_doesnt_take_over() {
    let folder = Folder::new();
    let mut store = sample();
    sync_now(&mut store, &folder);
    // Copies sort before and after the original.
    folder.put("people/a-copy.md", &folder.read(ANNA));
    folder.put("people/z-copy.md", &folder.read(ANNA));
    let later = episode(&mut store, at(11, 9), "Mail", "Ringde Anna.");
    mention(&mut store, later, "Anna Example");
    sync_now(&mut store, &folder);
    assert!(folder.read(ANNA).contains("Ringde Anna."));
    assert!(!folder.read("people/a-copy.md").contains("Ringde Anna."));
    assert_eq!(store.pages().unwrap()[0].path, ANNA);
}

#[test]
fn only_the_latest_versions_of_laxs_are_kept() {
    let folder = Folder::new();
    let mut store = sample();
    sync_now(&mut store, &folder);
    for minute in 0..25 {
        let when = at(11, 8) + TimeDelta::minutes(minute);
        let id = episode(
            &mut store,
            when,
            "Mail",
            &format!("Möte {minute} med Anna."),
        );
        mention(&mut store, id, "Anna Example");
        sync_now(&mut store, &folder);
    }
    let id = store.pages().unwrap()[0].id.clone();
    assert_eq!(
        store.page_versions(&id).unwrap(),
        (crate::store::LAX_VERSIONS_KEPT as usize, 0)
    );
    // The latest is still the one edits are compared with.
    assert!(
        store.pages().unwrap()[0]
            .last_block
            .as_deref()
            .unwrap()
            .contains("Möte 24")
    );
}

#[test]
fn unchanged_files_are_not_read_again() {
    let folder = Folder::new();
    let mut store = sample();
    sync_now(&mut store, &folder);
    let mut cache = ScanCache::new();
    assert_eq!(scan(&folder.0, &mut cache).len(), 1);
    // Make the file unreadable: a cached scan still knows it.
    fs::set_permissions(folder.0.join(ANNA), Permissions::from_mode(0o000)).unwrap();
    assert_eq!(scan(&folder.0, &mut cache).len(), 1);
    fs::set_permissions(folder.0.join(ANNA), Permissions::from_mode(0o600)).unwrap();
}

fn anna_page(store: &Store) -> crate::store::Page {
    store.pages().unwrap().remove(0)
}

fn facts(list: &[&str]) -> Vec<String> {
    list.iter().map(|f| f.to_string()).collect()
}

#[test]
fn facts_lead_the_block_and_deleted_ones_never_come_back() {
    let folder = Folder::new();
    let mut store = sample();
    sync_now(&mut store, &folder);
    let page = anna_page(&store);
    let sources: Vec<i64> = store
        .episodes_mentioning(&[page.entity_id.unwrap()], None, 10)
        .unwrap()
        .iter()
        .map(|e| e.id)
        .collect();
    assert!(
        store
            .set_facts(
                &page.id,
                &facts(&["Leder Q3-lanseringen.", "Jobbar på Exempelbolaget AB."]),
                &sources
            )
            .unwrap()
    );
    sync_now(&mut store, &folder);
    let text = folder.read(ANNA);
    assert!(
        text.contains(
            "-->\n- Leder Q3-lanseringen.\n- Jobbar på Exempelbolaget AB.\n\nMentioned in"
        )
    );

    // The user deletes a fact: rejected, gone from the page, and kept out.
    folder.put(ANNA, &text.replace("- Jobbar på Exempelbolaget AB.\n", ""));
    assert_eq!(sync_now(&mut store, &folder).rejected, 1);
    assert_eq!(
        store.rejected_facts(&page.id).unwrap(),
        ["Jobbar på Exempelbolaget AB."]
    );
    assert_eq!(anna_page(&store).facts.unwrap(), ["Leder Q3-lanseringen."]);
    let text = folder.read(ANNA);
    assert!(!text.contains("Exempelbolaget"));
    // Not adopted as the user's either.
    assert!(!text[..text.find("<!-- lax:begin").unwrap()].contains("Leder"));
    assert_eq!(sync_now(&mut store, &folder), Synced::default());
}

#[test]
fn facts_go_when_an_episode_they_drew_on_is_forgotten() {
    let folder = Folder::new();
    let mut store = sample();
    sync_now(&mut store, &folder);
    let page = anna_page(&store);
    let sources: Vec<i64> = store
        .episodes_mentioning(&[page.entity_id.unwrap()], None, 10)
        .unwrap()
        .iter()
        .map(|e| e.id)
        .collect();
    store
        .set_facts(&page.id, &facts(&["Leder Q3-lanseringen."]), &sources)
        .unwrap();
    sync_now(&mut store, &folder);
    assert!(folder.read(ANNA).contains("Leder Q3-lanseringen."));

    store
        .forget(&Forget::App("com.example.mail".to_string()))
        .unwrap();
    let page = anna_page(&store);
    assert_eq!(page.facts, None);
    sync_now(&mut store, &folder);
    assert!(!folder.read(ANNA).contains("Leder Q3-lanseringen."));
    // And the page is up for rebuilding.
    assert_eq!(store.stale_pages(5).unwrap().len(), 1);
}

#[test]
fn facts_from_a_forgotten_episode_are_refused() {
    let folder = Folder::new();
    let mut store = sample();
    sync_now(&mut store, &folder);
    let page = anna_page(&store);
    assert!(
        !store
            .set_facts(&page.id, &facts(&["Gammalt."]), &[999])
            .unwrap()
    );
    assert_eq!(anna_page(&store).facts, None);
}

#[test]
fn pages_are_stale_until_their_facts_have_seen_every_mention() {
    let folder = Folder::new();
    let mut store = sample();
    sync_now(&mut store, &folder);
    assert_eq!(store.stale_pages(5).unwrap().len(), 1);
    let page = anna_page(&store);
    let sources: Vec<i64> = store
        .episodes_mentioning(&[page.entity_id.unwrap()], None, 10)
        .unwrap()
        .iter()
        .map(|e| e.id)
        .collect();
    store
        .set_facts(&page.id, &facts(&["En sak."]), &sources)
        .unwrap();
    assert!(store.stale_pages(5).unwrap().is_empty());
    let later = episode(&mut store, at(11, 9), "Mail", "Ringde Anna.");
    mention(&mut store, later, "Anna Example");
    assert_eq!(store.stale_pages(5).unwrap().len(), 1);
}

#[test]
fn mentions_merged_in_from_older_episodes_make_a_page_stale() {
    let folder = Folder::new();
    let mut store = sample();
    sync_now(&mut store, &folder);
    let page = anna_page(&store);
    let anna = page.entity_id.unwrap();
    let sources: Vec<i64> = store
        .episodes_mentioning(&[anna], None, 10)
        .unwrap()
        .iter()
        .map(|e| e.id)
        .collect();
    store
        .set_facts(&page.id, &facts(&["En sak."]), &sources)
        .unwrap();
    assert!(store.stale_pages(5).unwrap().is_empty());
    // Bo's single, older mention becomes Anna's.
    let bo = store
        .entities()
        .unwrap()
        .into_iter()
        .find(|e| e.name == "Bo Exempel")
        .unwrap()
        .id;
    store.merge_entities(bo, anna).unwrap();
    assert_eq!(store.stale_pages(5).unwrap().len(), 1);
    let unseen = store.unseen_mentions(&page.id, anna, 10).unwrap();
    assert_eq!(unseen.len(), 1);
}

#[test]
fn name_clashes_get_a_number() {
    let folder = Folder::new();
    fs::create_dir_all(folder.0.join("people")).unwrap();
    folder.put(ANNA, "someone else's");
    assert_eq!(
        free_path(&folder.0, EntityKind::Person, "Anna Example"),
        "people/anna-example-2.md"
    );
}
