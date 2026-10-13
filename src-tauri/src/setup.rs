//! The first start: after the Accessibility permission, a short guide to a
//! local model and to what to turn on. Shown until the user finishes it, and
//! only while no chat model is chosen, so earlier installs skip it.

use std::sync::{Mutex, PoisonError};

use serde::Serialize;
use tauri::{AppHandle, Manager};

use crate::store::{ModelTask, Store};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupStatus {
    /// Whether to show the guide.
    needed: bool,
    /// The Mac's memory in whole gigabytes, for model advice; 0 if unknown.
    memory_gb: u64,
}

/// Whether to show the guide: not finished, and no chat model chosen yet.
pub fn needed(finished: bool, has_chat_model: bool) -> bool {
    !finished && !has_chat_model
}

#[tauri::command(async)]
pub fn setup_status(app: AppHandle) -> Result<SetupStatus, String> {
    let store = app
        .try_state::<Mutex<Store>>()
        .ok_or("The database isn't available.")?;
    let store = store.lock().unwrap_or_else(PoisonError::into_inner);
    let failed = |_| "Lax couldn't read its settings.".to_string();
    let finished = store.setup_finished().map_err(failed)?;
    let chat = store.model_choice(ModelTask::Chat).map_err(failed)?;
    Ok(SetupStatus {
        needed: needed(finished, chat.is_some()),
        memory_gb: memory_gb(),
    })
}

#[tauri::command(async)]
pub fn finish_setup(app: AppHandle) -> Result<(), String> {
    let store = app
        .try_state::<Mutex<Store>>()
        .ok_or("The database isn't available.")?;
    let store = store.lock().unwrap_or_else(PoisonError::into_inner);
    store
        .set_setup_finished()
        .map_err(|_| "Lax couldn't save the setting.".to_string())
}

/// The Mac's physical memory in whole gigabytes.
fn memory_gb() -> u64 {
    #[cfg(target_os = "macos")]
    {
        let bytes = objc2_foundation::NSProcessInfo::processInfo().physicalMemory();
        bytes / (1024 * 1024 * 1024)
    }
    #[cfg(not(target_os = "macos"))]
    {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_guide_shows_only_on_a_first_start() {
        assert!(needed(false, false));
        // Finished, or a model chosen earlier (an existing install).
        assert!(!needed(true, false));
        assert!(!needed(false, true));
    }

    #[test]
    fn memory_can_be_read() {
        // Any Mac running the tests has some.
        #[cfg(target_os = "macos")]
        assert!(memory_gb() >= 1);
    }
}
