//! Whether FileVault is on (GUARDRAILS §6).
//!
//! Lax relies on FileVault to encrypt what it stores, and warns when it is off.
//! The check runs `fdesetup status`, which only reads the status and needs no
//! administrator rights.

use std::process::Command;

/// `Some(true)` if FileVault is on, `Some(false)` if it is off or being turned
/// off, `None` if the status couldn't be read.
#[tauri::command(async)]
pub fn filevault_enabled() -> Option<bool> {
    let output = Command::new("/usr/bin/fdesetup")
        .arg("status")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_status(&String::from_utf8_lossy(&output.stdout))
}

fn parse_status(stdout: &str) -> Option<bool> {
    // Being turned off counts as off, whatever the first line says.
    if stdout.contains("FileVault is Off") || stdout.contains("Decryption in progress") {
        Some(false)
    } else if stdout.contains("FileVault is On") {
        // Includes "Encryption in progress", which ends with the disk encrypted.
        Some(true)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn on() {
        assert_eq!(parse_status("FileVault is On.\n"), Some(true));
        assert_eq!(
            parse_status("FileVault is On.\nEncryption in progress: Percent completed = 42\n"),
            Some(true)
        );
    }

    #[test]
    fn off() {
        assert_eq!(parse_status("FileVault is Off.\n"), Some(false));
        assert_eq!(
            parse_status("FileVault is On.\nDecryption in progress: Percent completed = 42\n"),
            Some(false)
        );
    }

    #[test]
    fn unknown() {
        assert_eq!(parse_status(""), None);
        assert_eq!(parse_status("Error: something else\n"), None);
    }
}
