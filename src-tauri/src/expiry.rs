//! Deletes raw captures older than the retention period (GUARDRAILS §4), at
//! startup and then every hour.
//!
//! This only deletes, so it also runs while capture is paused.

use std::sync::{Mutex, PoisonError};
use std::thread;
use std::time::Duration;

use chrono::Utc;
use tauri::{AppHandle, Manager};

use crate::store::Store;

const INTERVAL: Duration = Duration::from_secs(60 * 60);

pub fn start(app: AppHandle) {
    thread::spawn(move || {
        loop {
            if let Some(store) = app.try_state::<Mutex<Store>>() {
                let store = store.lock().unwrap_or_else(PoisonError::into_inner);
                // A failure is retried at the next run. Nothing is logged
                // until the logging policy (#13) is in place.
                let _ = store.expire(Utc::now());
            }
            thread::sleep(INTERVAL);
        }
    });
}
