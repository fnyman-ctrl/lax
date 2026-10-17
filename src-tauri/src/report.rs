//! "Copy details for a bug report" (#171): what a tester is asked for in an
//! issue, gathered for them. Versions, the Mac, the models and their server,
//! which features are on, and counts. Never anything captured, no names, no
//! page text, no window titles, no paths, no addresses or keys. Nothing is
//! sent: the window puts the text on the clipboard, and the user pastes it.

use std::sync::{Mutex, PoisonError};

use tauri::{AppHandle, Manager};

use crate::store::{DATABASE_FILE, ModelTask, Store};

/// What goes into the details, all of it safe to paste in public.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Details {
    pub version: &'static str,
    pub commit: &'static str,
    pub macos: String,
    pub mac_model: String,
    pub memory_gb: u64,
    /// Model name and the kind of server, never its address.
    pub chat: Option<(String, &'static str)>,
    pub embeddings: Option<(String, &'static str)>,
    pub journal: bool,
    pub memory: bool,
    pub writing: bool,
    pub entries: usize,
    pub names: usize,
    pub pages: usize,
    pub database_mb: u64,
}

impl Details {
    pub fn text(&self) -> String {
        let on = |b: bool| if b { "on" } else { "off" };
        let model = |m: &Option<(String, &'static str)>| match m {
            Some((name, server)) => format!("{name} on {server}"),
            None => "none".to_string(),
        };
        format!(
            "Lax {} (commit {})\n\
             macOS {}, {}, {} GB memory\n\
             Chat model: {}\n\
             Embedding model: {}\n\
             Journal: {}. Memory: {}. Writing assistant: {}.\n\
             Journal entries: {}. Names: {}. Pages: {}. Database: {} MB.\n",
            self.version,
            self.commit,
            self.macos,
            self.mac_model,
            self.memory_gb,
            model(&self.chat),
            model(&self.embeddings),
            on(self.journal),
            on(self.memory),
            on(self.writing),
            self.entries,
            self.names,
            self.pages,
            self.database_mb,
        )
    }
}

/// The kind of model server at `base_url`, by its usual port: the address
/// itself never goes into the details.
fn server_kind(base_url: &str) -> &'static str {
    let port = base_url
        .split("://")
        .nth(1)
        .and_then(|rest| rest.split('/').next())
        .and_then(|host| host.rsplit_once(':'))
        .map(|(_, port)| port);
    match port {
        Some("1234") => "LM Studio",
        Some("11434") => "Ollama",
        _ => "another local server",
    }
}

/// Everything from the database: models, switches and counts.
fn from_store(store: &Store, details: &mut Details) -> crate::store::Result<()> {
    let model = |task| -> crate::store::Result<Option<(String, &'static str)>> {
        Ok(store
            .model_choice(task)?
            .map(|c| (c.model, server_kind(&c.base_url))))
    };
    details.chat = model(ModelTask::Chat)?;
    details.embeddings = model(ModelTask::Embeddings)?;
    details.journal = store.journal_enabled()?;
    details.memory = store.memory_enabled()?;
    details.writing = store.writing_enabled()?;
    (details.entries, details.names, details.pages) = store.report_counts()?;
    Ok(())
}

/// The details as text, for the clipboard.
#[tauri::command(async)]
pub fn bug_report_details(app: AppHandle) -> Result<String, String> {
    let mut details = Details {
        version: env!("CARGO_PKG_VERSION"),
        commit: env!("LAX_COMMIT"),
        macos: macos_version(),
        mac_model: mac_model(),
        memory_gb: memory_gb(),
        ..Details::default()
    };
    let store = app
        .try_state::<Mutex<Store>>()
        .ok_or_else(|| "The database isn't available.".to_string())?;
    {
        let store = store.lock().unwrap_or_else(PoisonError::into_inner);
        from_store(&store, &mut details)
            .map_err(|_| "Lax couldn't read its settings.".to_string())?;
    }
    if let Ok(dir) = app.path().app_data_dir() {
        let bytes: u64 = ["", "-wal"]
            .iter()
            .filter_map(|suffix| {
                std::fs::metadata(dir.join(format!("{DATABASE_FILE}{suffix}"))).ok()
            })
            .map(|m| m.len())
            .sum();
        details.database_mb = bytes / (1024 * 1024);
    }
    Ok(details.text())
}

fn macos_version() -> String {
    #[cfg(target_os = "macos")]
    {
        let v = objc2_foundation::NSProcessInfo::processInfo().operatingSystemVersion();
        format!("{}.{}.{}", v.majorVersion, v.minorVersion, v.patchVersion)
    }
    #[cfg(not(target_os = "macos"))]
    {
        "unknown".to_string()
    }
}

/// The Mac's model identifier, such as "MacBookPro18,1".
#[allow(unsafe_code)] // One sysctl call; see SAFETY below.
fn mac_model() -> String {
    let mut buffer = [0u8; 64];
    let mut size = buffer.len();
    // SAFETY: a valid, NUL-terminated name; the buffer and its size are
    // valid, and sysctl writes at most `size` bytes.
    let result = unsafe {
        libc::sysctlbyname(
            c"hw.model".as_ptr(),
            buffer.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if result != 0 {
        return "unknown".to_string();
    }
    String::from_utf8_lossy(&buffer[..size])
        .trim_end_matches('\0')
        .to_string()
}

fn memory_gb() -> u64 {
    #[cfg(target_os = "macos")]
    {
        objc2_foundation::NSProcessInfo::processInfo().physicalMemory() / (1024 * 1024 * 1024)
    }
    #[cfg(not(target_os = "macos"))]
    {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{EntityKind, NewCapture, NewEpisode};

    #[test]
    fn servers_are_named_by_kind_never_by_address() {
        assert_eq!(server_kind("http://localhost:1234/v1"), "LM Studio");
        assert_eq!(server_kind("http://127.0.0.1:11434/v1"), "Ollama");
        assert_eq!(
            server_kind("http://my-mac.example.com:8080/v1"),
            "another local server"
        );
    }

    /// From a database with synthetic captures, entries, names and pages,
    /// the details hold counts, and none of what was captured or named.
    #[test]
    fn the_details_hold_nothing_captured() {
        let mut store = Store::open_in_memory().unwrap();
        let when = chrono::Utc::now();
        let capture = store
            .insert(&NewCapture {
                captured_at: when,
                bundle_id: "com.example.mail",
                app_name: "Mail",
                window_title: "Offert till Anna Example",
                site: None,
                text: "Hej Anna Example, här är offerten för Q3 lansering.",
            })
            .unwrap()
            .unwrap();
        let episode = store
            .record_episode(
                &NewEpisode {
                    summary: "Mailade Anna Example om Q3 lansering.".to_string(),
                    model: "example-model".to_string(),
                    created_at: when,
                },
                &[capture],
            )
            .unwrap()
            .unwrap();
        store
            .record_mentions(
                episode,
                &[(EntityKind::Person, "Anna Example".to_string())],
                when,
            )
            .unwrap();
        store
            .set_model_choice(
                ModelTask::Chat,
                Some(&crate::store::ModelChoice {
                    base_url: "http://localhost:1234/v1".to_string(),
                    model: "google/gemma-4-e4b".to_string(),
                }),
            )
            .unwrap();
        let mut details = Details::default();
        from_store(&store, &mut details).unwrap();
        let text = details.text();
        for word in [
            "Anna",
            "Offert",
            "offerten",
            "Q3",
            "Mail",
            "localhost",
            "1234",
        ] {
            assert!(
                !text.contains(word),
                "a captured word is in the details: {word}"
            );
        }
        assert!(text.contains("Chat model: google/gemma-4-e4b on LM Studio"));
        assert!(text.contains("Journal entries: 1. Names: 1."));
    }

    /// On a real Mac: the system details read as something sensible.
    #[test]
    #[ignore = "reads this Mac's model and macOS version"]
    fn live_system_details() {
        assert!(mac_model().contains(','), "{}", mac_model());
        assert!(macos_version().split('.').count() == 3);
        assert!(memory_gb() >= 4);
        assert_ne!(env!("LAX_COMMIT"), "unknown");
    }
}
