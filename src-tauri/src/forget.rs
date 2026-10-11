//! Forgetting and deleting from the window (GUARDRAILS §1, §4): the last
//! minutes, a time range, an app, or everything, plus how long raw captures
//! are kept. Deletion takes effect immediately and is gone from the files on
//! disk too (see `Store::forget`).

use std::sync::{Mutex, PoisonError};

use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::logging::{self, ErrorKind, Field};
use crate::store::{CapturedApp, Forget, MAX_RETENTION_DAYS, Store};

/// The longest "last minutes" the window can ask for: one week.
const MAX_LAST_MINUTES: u32 = 7 * 24 * 60;

/// What the window asks to forget.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ForgetRequest {
    /// The last `minutes` minutes, up to now.
    Last { minutes: u32 },
    /// From `from` up to `to`, Unix time in milliseconds.
    Range { from: i64, to: i64 },
    /// Everything from one app.
    App { bundle_id: String },
}

impl ForgetRequest {
    fn to_forget(&self, now: DateTime<Utc>) -> Result<Forget, String> {
        match self {
            ForgetRequest::Last { minutes } => {
                if !(1..=MAX_LAST_MINUTES).contains(minutes) {
                    return Err("Choose between a minute and a week.".to_string());
                }
                Ok(Forget::Since(now - TimeDelta::minutes(i64::from(*minutes))))
            }
            ForgetRequest::Range { from, to } => {
                let from = DateTime::from_timestamp_millis(*from).ok_or("Invalid start time.")?;
                let to = DateTime::from_timestamp_millis(*to).ok_or("Invalid end time.")?;
                if from >= to {
                    return Err("The start must be before the end.".to_string());
                }
                Ok(Forget::Between(from, to))
            }
            ForgetRequest::App { bundle_id } => {
                let bundle_id = bundle_id.trim();
                if bundle_id.is_empty() {
                    return Err("Choose an app.".to_string());
                }
                Ok(Forget::App(bundle_id.to_string()))
            }
        }
    }
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AppItem {
    pub bundle_id: String,
    pub app_name: String,
    pub captures: usize,
}

impl From<CapturedApp> for AppItem {
    fn from(app: CapturedApp) -> Self {
        AppItem {
            bundle_id: app.bundle_id,
            app_name: app.app_name,
            captures: app.captures,
        }
    }
}

/// Apps with captures, for "forget this app".
#[tauri::command(async)]
pub fn captured_apps(app: AppHandle) -> Result<Vec<AppItem>, String> {
    with_store(&app, |store| store.apps()).map(|apps| apps.into_iter().map(AppItem::from).collect())
}

/// Forgets what `what` covers and returns how many captures were deleted.
#[tauri::command(async)]
pub fn forget_captures(app: AppHandle, what: ForgetRequest) -> Result<usize, String> {
    let forget = what.to_forget(Utc::now())?;
    let deleted = with_store(&app, |store| store.forget(&forget))?;
    logging::info("forget.done", &[Field::Count("deleted", deleted as u64)]);
    // What the memory's pages drew from the forgotten episodes goes too.
    crate::pages::sync_now(&app);
    Ok(deleted)
}

/// Deletes every capture and the logs, whose timestamps show when Lax was
/// in use. Settings (retention, pause) are kept: they say nothing about what
/// was on screen.
#[tauri::command(async)]
pub fn delete_all_data(app: AppHandle) -> Result<usize, String> {
    let deleted = with_store(&app, |store| store.forget(&Forget::Everything))?;
    crate::pages::sync_now(&app);
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|_| "Lax couldn't find its data folder.".to_string())?;
    if logging::delete_all(&data_dir).is_err() {
        logging::warn("forget.logs_failed", &[Field::Error(ErrorKind::Io)]);
        return Err("The captures were deleted, but the logs couldn't be.".to_string());
    }
    Ok(deleted)
}

#[tauri::command(async)]
pub fn retention_days(app: AppHandle) -> Result<u32, String> {
    with_store(&app, |store| store.retention_days())
}

/// Changes how long raw captures are kept, and deletes older ones now.
#[tauri::command(async)]
pub fn set_retention_days(app: AppHandle, days: u32) -> Result<(), String> {
    if !(1..=MAX_RETENTION_DAYS).contains(&days) {
        return Err(format!("Choose between 1 and {MAX_RETENTION_DAYS} days."));
    }
    with_store(&app, |store| store.set_retention_days(days, Utc::now()))
}

/// Runs `change` on the store. Errors become a fixed message: they are never
/// shown or logged with their details.
fn with_store<T>(
    app: &AppHandle,
    change: impl FnOnce(&Store) -> crate::store::Result<T>,
) -> Result<T, String> {
    let store = app
        .try_state::<Mutex<Store>>()
        .ok_or("The database isn't available.")?;
    let store = store.lock().unwrap_or_else(PoisonError::into_inner);
    change(&store).map_err(|_| {
        logging::warn("forget.failed", &[Field::Error(ErrorKind::Database)]);
        "Lax couldn't change the database. Nothing was deleted.".to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp_millis(1_781_520_000_000).unwrap()
    }

    fn parse(json: &str) -> ForgetRequest {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn requests_arrive_as_the_window_sends_them() {
        assert_eq!(
            parse(r#"{"kind":"last","minutes":15}"#),
            ForgetRequest::Last { minutes: 15 }
        );
        assert_eq!(
            parse(r#"{"kind":"range","from":1,"to":2}"#),
            ForgetRequest::Range { from: 1, to: 2 }
        );
        assert_eq!(
            parse(r#"{"kind":"app","bundleId":"com.apple.mail"}"#),
            ForgetRequest::App {
                bundle_id: "com.apple.mail".to_string()
            }
        );
        assert!(serde_json::from_str::<ForgetRequest>(r#"{"kind":"everything"}"#).is_err());
    }

    #[test]
    fn last_minutes_reach_back_from_now() {
        let forget = ForgetRequest::Last { minutes: 60 }
            .to_forget(now())
            .unwrap();
        assert_eq!(forget, Forget::Since(now() - TimeDelta::hours(1)));
    }

    #[test]
    fn nonsense_requests_are_refused() {
        for request in [
            ForgetRequest::Last { minutes: 0 },
            ForgetRequest::Last {
                minutes: MAX_LAST_MINUTES + 1,
            },
            ForgetRequest::Range { from: 10, to: 10 },
            ForgetRequest::Range { from: 20, to: 10 },
            ForgetRequest::Range {
                from: i64::MAX,
                to: i64::MAX,
            },
            ForgetRequest::App {
                bundle_id: "  ".to_string(),
            },
        ] {
            assert!(request.to_forget(now()).is_err(), "{request:?}");
        }
    }

    #[test]
    fn ranges_and_apps_map_directly() {
        let from = now() - TimeDelta::hours(2);
        let forget = ForgetRequest::Range {
            from: from.timestamp_millis(),
            to: now().timestamp_millis(),
        }
        .to_forget(now())
        .unwrap();
        assert_eq!(forget, Forget::Between(from, now()));
        let forget = ForgetRequest::App {
            bundle_id: " com.apple.mail ".to_string(),
        }
        .to_forget(now())
        .unwrap();
        assert_eq!(forget, Forget::App("com.apple.mail".to_string()));
    }
}
