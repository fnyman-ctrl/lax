//! What the window shows: capture status, the timeline of captures and search
//! (GUARDRAILS §1: you can see everything that was captured).
//!
//! The web UI only gets what these commands return. Captured text is plain
//! data to it, never HTML or instructions (GUARDRAILS §5).

use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use chrono::{DateTime, Local};
use serde::Serialize;
use tauri::{AppHandle, Manager};

use crate::accessibility;
use crate::capture::Heartbeat;
use crate::pause::Pause;
use crate::store::{Capture, CaptureSummary, DEFAULT_CAPTURE_INTERVAL_SECONDS, SearchHit, Store};

/// The most captures one request can return.
const MAX_LIMIT: u32 = 200;
/// The longest search query, in characters.
const MAX_QUERY_CHARS: usize = 200;

/// Set when the database couldn't be opened, so the window can say why.
pub struct StoreUnavailable(pub String);

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CaptureStatus {
    pub paused: bool,
    /// The same line as in the menu bar ("Paused until 14:15").
    pub label: String,
    pub permission: bool,
    /// Whether the capture loop is going round. If not, nothing is captured
    /// whatever the pause and the permission say.
    pub capturing: bool,
    /// Why nothing can be stored, if the database couldn't be opened.
    pub store_error: Option<String>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CaptureItem {
    pub id: i64,
    /// Unix time in milliseconds.
    pub captured_at: i64,
    pub app_name: String,
    pub bundle_id: String,
    pub window_title: String,
    pub site: Option<String>,
    pub preview: String,
    pub text_chars: usize,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Hit {
    pub capture: CaptureItem,
    pub snippet: String,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CaptureDetail {
    pub id: i64,
    pub captured_at: i64,
    pub app_name: String,
    pub window_title: String,
    pub site: Option<String>,
    pub text: String,
}

impl From<CaptureSummary> for CaptureItem {
    fn from(c: CaptureSummary) -> Self {
        CaptureItem {
            id: c.id,
            captured_at: c.captured_at.timestamp_millis(),
            app_name: c.app_name,
            bundle_id: c.bundle_id,
            window_title: c.window_title,
            site: c.site,
            preview: c.preview,
            text_chars: c.text_chars,
        }
    }
}

impl From<SearchHit> for Hit {
    fn from(hit: SearchHit) -> Self {
        Hit {
            capture: hit.capture.into(),
            snippet: hit.snippet,
        }
    }
}

impl From<Capture> for CaptureDetail {
    fn from(c: Capture) -> Self {
        CaptureDetail {
            id: c.id,
            captured_at: c.captured_at.timestamp_millis(),
            app_name: c.app_name,
            window_title: c.window_title,
            site: c.site,
            text: c.text,
        }
    }
}

#[tauri::command(async)]
pub fn capture_status(app: AppHandle) -> CaptureStatus {
    let state = app.state::<Pause>().state();
    let interval = app
        .try_state::<Mutex<Store>>()
        .and_then(|store| {
            let store = store.lock().unwrap_or_else(PoisonError::into_inner);
            store.capture_interval_seconds().ok()
        })
        .unwrap_or(DEFAULT_CAPTURE_INTERVAL_SECONDS);
    // A few rounds missed, plus room for a slow window.
    let max_gap = Duration::from_secs(u64::from(interval) * 3 + 10);
    CaptureStatus {
        paused: state.is_paused(),
        label: state.label(&Local::now()),
        permission: accessibility::is_granted(),
        capturing: app.state::<Heartbeat>().alive(max_gap),
        store_error: app
            .try_state::<StoreUnavailable>()
            .map(|unavailable| unavailable.0.clone()),
    }
}

/// Captures from newest to oldest. To continue, pass the time and id of the
/// last capture shown as `before_at` and `before_id`.
#[tauri::command(async)]
pub fn recent_captures(
    app: AppHandle,
    before_at: Option<i64>,
    before_id: Option<i64>,
    limit: u32,
) -> Result<Vec<CaptureItem>, String> {
    let before = match (before_at, before_id) {
        (Some(at), Some(id)) => Some((
            DateTime::from_timestamp_millis(at).ok_or("Invalid time")?,
            id,
        )),
        _ => None,
    };
    with_store(&app, |store| store.recent(before, clamp(limit)))
        .map(|captures| captures.into_iter().map(CaptureItem::from).collect())
}

#[tauri::command(async)]
pub fn search_captures(app: AppHandle, query: String, limit: u32) -> Result<Vec<Hit>, String> {
    let query: String = query.chars().take(MAX_QUERY_CHARS).collect();
    with_store(&app, |store| store.search_hits(&query, clamp(limit)))
        .map(|hits| hits.into_iter().map(Hit::from).collect())
}

#[tauri::command(async)]
pub fn capture_detail(app: AppHandle, id: i64) -> Result<Option<CaptureDetail>, String> {
    with_store(&app, |store| store.get(id)).map(|capture| capture.map(CaptureDetail::from))
}

fn clamp(limit: u32) -> usize {
    limit.clamp(1, MAX_LIMIT) as usize
}

/// Runs `read` on the store. Errors become a fixed message: they can name
/// files and settings, but never captured content, and they are not logged.
fn with_store<T>(
    app: &AppHandle,
    read: impl FnOnce(&Store) -> crate::store::Result<T>,
) -> Result<T, String> {
    let store = app
        .try_state::<Mutex<Store>>()
        .ok_or("The database isn't available.")?;
    let store = store.lock().unwrap_or_else(PoisonError::into_inner);
    read(&store).map_err(|_| "Lax couldn't read the database.".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_web_ui_never_renders_text_as_html() {
        // Captured text is untrusted (GUARDRAILS §5). React escapes text, so
        // rendering it as HTML takes one of these, and none may be used.
        let forbidden = [
            "dangerouslySetInnerHTML",
            "innerHTML",
            "outerHTML",
            "insertAdjacentHTML",
            "document.write",
            "srcDoc",
            "srcdoc",
            "DOMParser",
            "createContextualFragment",
        ];
        let ui = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../src");
        let mut dirs = vec![ui];
        let mut checked = 0;
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    dirs.push(path);
                } else if path.extension().is_some_and(|e| e == "ts" || e == "tsx") {
                    let source = std::fs::read_to_string(&path).unwrap();
                    for name in forbidden {
                        assert!(!source.contains(name), "{} uses {name}", path.display());
                    }
                    checked += 1;
                }
            }
        }
        assert!(checked > 0, "no web UI sources found");
    }

    #[test]
    fn limits_are_clamped() {
        assert_eq!(clamp(0), 1);
        assert_eq!(clamp(50), 50);
        assert_eq!(clamp(u32::MAX), MAX_LIMIT as usize);
    }

    #[test]
    fn items_carry_milliseconds_and_the_preview() {
        let at = DateTime::from_timestamp_millis(1_781_520_000_000).unwrap();
        let item = CaptureItem::from(CaptureSummary {
            id: 7,
            captured_at: at,
            app_name: "Mail".into(),
            bundle_id: "com.apple.mail".into(),
            window_title: "Inbox".into(),
            site: None,
            preview: "Hej Anna".into(),
            text_chars: 8,
        });
        assert_eq!(item.captured_at, 1_781_520_000_000);
        assert_eq!(item.preview, "Hej Anna");
        let json = serde_json::to_value(&item).unwrap();
        assert!(json.get("capturedAt").is_some(), "{json}");
        assert!(json.get("textChars").is_some(), "{json}");
    }
}
