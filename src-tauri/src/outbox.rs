//! The outbox in the window (GUARDRAILS §1, §3, #19): everything Lax has sent
//! to a remote model provider, exactly as sent, to read, search and delete.
//!
//! Remote providers arrive with consent (#16); until then the outbox stays
//! empty, and the window says so.

use std::sync::{Mutex, PoisonError};

use chrono::{DateTime, Utc};
use serde::Serialize;
use tauri::{AppHandle, Manager};

use crate::store::{MAX_RETENTION_DAYS, OutboxSummary, Store};

const MAX_LIMIT: u32 = 200;
const MAX_QUERY_CHARS: usize = 200;

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OutboxItem {
    pub id: i64,
    pub sent_at: i64,
    pub provider: String,
    pub task: String,
    pub model: String,
    pub preview: String,
    pub payload_chars: usize,
    pub status: u16,
    pub response_bytes: usize,
}

impl From<OutboxSummary> for OutboxItem {
    fn from(o: OutboxSummary) -> Self {
        OutboxItem {
            id: o.id,
            sent_at: o.sent_at.timestamp_millis(),
            provider: o.provider,
            task: o.task,
            model: o.model,
            preview: o.preview,
            payload_chars: o.payload_chars,
            status: o.status,
            response_bytes: o.response_bytes,
        }
    }
}

#[tauri::command(async)]
pub fn outbox_entries(
    app: AppHandle,
    query: Option<String>,
    before_at: Option<i64>,
    before_id: Option<i64>,
    limit: u32,
) -> Result<Vec<OutboxItem>, String> {
    let query: Option<String> = query
        .map(|q| q.chars().take(MAX_QUERY_CHARS).collect::<String>())
        .filter(|q| !q.trim().is_empty());
    let before = match (before_at, before_id) {
        (Some(at), Some(id)) => Some((
            DateTime::<Utc>::from_timestamp_millis(at).ok_or("Invalid time")?,
            id,
        )),
        _ => None,
    };
    let limit = limit.clamp(1, MAX_LIMIT) as usize;
    with_store(&app, |store| store.outbox(query.as_deref(), before, limit))
        .map(|items| items.into_iter().map(OutboxItem::from).collect())
}

#[tauri::command(async)]
pub fn outbox_payload(app: AppHandle, id: i64) -> Result<Option<String>, String> {
    with_store(&app, |store| store.outbox_payload(id))
}

/// Deletes one entry, or with no `id` the whole outbox.
#[tauri::command(async)]
pub fn delete_outbox(app: AppHandle, id: Option<i64>) -> Result<usize, String> {
    with_store(&app, |store| store.delete_outbox(id))
}

#[tauri::command(async)]
pub fn outbox_retention_days(app: AppHandle) -> Result<u32, String> {
    with_store(&app, |store| store.outbox_retention_days())
}

#[tauri::command(async)]
pub fn set_outbox_retention_days(app: AppHandle, days: u32) -> Result<(), String> {
    if !(1..=MAX_RETENTION_DAYS).contains(&days) {
        return Err(format!("Choose between 1 and {MAX_RETENTION_DAYS} days."));
    }
    with_store(&app, |store| {
        store.set_outbox_retention_days(days, Utc::now())
    })
}

fn with_store<T>(
    app: &AppHandle,
    work: impl FnOnce(&Store) -> crate::store::Result<T>,
) -> Result<T, String> {
    let store = app
        .try_state::<Mutex<Store>>()
        .ok_or("The database isn't available.")?;
    let store = store.lock().unwrap_or_else(PoisonError::into_inner);
    work(&store).map_err(|_| "Lax couldn't read or change the outbox.".to_string())
}
