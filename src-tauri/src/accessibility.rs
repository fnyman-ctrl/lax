//! The macOS Accessibility permission, which capture needs to read window text.
//!
//! Lax only checks whether the permission is granted, and opens System Settings
//! when the user asks. It never shows the system prompt by itself, and never
//! tries to grant the permission or work around it.

use std::process::Command;
use std::thread;
use std::time::Duration;

use tauri::{AppHandle, Emitter};

/// Sent to the web UI with the new state (`true` = granted) when it changes.
pub const CHANGED_EVENT: &str = "accessibility-changed";

/// The Accessibility list in System Settings → Privacy & Security.
const SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility";

/// How often to check whether the user granted or revoked the permission.
/// macOS sends no notification for this. While it's missing, check often so
/// the window updates soon after the user turns it on; once granted, a
/// revoke only needs to be noticed within a few seconds.
const POLL_WHILE_MISSING: Duration = Duration::from_secs(2);
const POLL_WHILE_GRANTED: Duration = Duration::from_secs(10);

#[cfg(target_os = "macos")]
#[allow(unsafe_code)] // The only foreign declaration here; see SAFETY below.
mod ffi {
    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        // SAFETY: `Boolean AXIsProcessTrusted(void)` takes no arguments, has no
        // preconditions and returns a plain byte, so calling it is always safe.
        // It only reads the permission and never prompts the user.
        pub safe fn AXIsProcessTrusted() -> u8;
    }
}

pub fn is_granted() -> bool {
    #[cfg(target_os = "macos")]
    {
        ffi::AXIsProcessTrusted() != 0
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

#[tauri::command]
pub fn accessibility_status() -> bool {
    is_granted()
}

/// Opens System Settings at the Accessibility list, where the user can turn
/// Lax on or off. The URL is fixed; nothing from the web UI is passed on.
#[tauri::command(async)]
pub fn open_accessibility_settings() -> Result<(), String> {
    Command::new("/usr/bin/open")
        .arg(SETTINGS_URL)
        .status()
        .ok()
        .filter(|status| status.success())
        .map(|_| ())
        .ok_or_else(|| "Could not open System Settings".to_string())
}

/// Watches for the permission being granted or revoked while Lax runs,
/// starting from `granted`, the state the caller already acted on. Tells the
/// web UI and calls `on_change` with the new state.
pub fn watch(app: AppHandle, granted: bool, on_change: impl Fn(&AppHandle, bool) + Send + 'static) {
    thread::spawn(move || {
        let mut watcher = Watcher::new(is_granted, granted);
        loop {
            thread::sleep(if watcher.last {
                POLL_WHILE_GRANTED
            } else {
                POLL_WHILE_MISSING
            });
            if let Some(granted) = watcher.poll() {
                let _ = app.emit(CHANGED_EVENT, granted);
                on_change(&app, granted);
            }
        }
    });
}

/// Reports a change in a boolean check between polls.
struct Watcher<F> {
    check: F,
    last: bool,
}

impl<F: Fn() -> bool> Watcher<F> {
    fn new(check: F, last: bool) -> Self {
        Self { check, last }
    }

    /// The new state if it changed since the last poll.
    fn poll(&mut self) -> Option<bool> {
        let now = (self.check)();
        if now == self.last {
            None
        } else {
            self.last = now;
            Some(now)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn watcher_reports_only_changes() {
        let granted = Cell::new(false);
        let mut watcher = Watcher::new(|| granted.get(), false);
        assert_eq!(watcher.poll(), None);

        granted.set(true);
        assert_eq!(watcher.poll(), Some(true));
        assert_eq!(watcher.poll(), None);

        granted.set(false);
        assert_eq!(watcher.poll(), Some(false));
        assert_eq!(watcher.poll(), None);
    }

    #[test]
    fn watcher_starts_from_the_state_the_caller_saw() {
        // Revoked between the startup check and the first poll: reported.
        let mut watcher = Watcher::new(|| false, true);
        assert_eq!(watcher.poll(), Some(false));
    }

    #[test]
    fn nothing_asks_macos_to_prompt_for_the_permission() {
        // The variant of the check that takes options can show the system
        // dialog. Lax must only ever use the plain check, in every file.
        let forbidden = ["AXIsProcessTrusted", "WithOptions"].concat();
        let mut dirs = vec![std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src")];
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    dirs.push(path);
                } else if path.extension().is_some_and(|ext| ext == "rs") {
                    let source = std::fs::read_to_string(&path).unwrap();
                    assert!(!source.contains(&forbidden), "{}", path.display());
                }
            }
        }
    }
}
