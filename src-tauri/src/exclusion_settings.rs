//! The user's exclusions in the window (GUARDRAILS §4): their own apps,
//! websites and title patterns (tier 3), switches for the banking and health
//! defaults (tier 2), and the list of what is always excluded (tier 1, not
//! switchable). Every change is stored first, then handed to capture, which
//! applies it before its next read.

use std::sync::{Mutex, PoisonError};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::exclusions::{ALWAYS_EXCLUDED, Category, Current, DEFAULT_EXCLUSIONS, Exclusions};
use crate::logging::{self, ErrorKind, Field};
use crate::site::site_from_url;
use crate::store::{ExclusionKind, Store};

const MAX_BUNDLE_ID_CHARS: usize = 255;
const MAX_TITLE_PATTERN_CHARS: usize = 200;

/// The kinds the window can add or remove.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Kind {
    App,
    Site,
    Title,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ExclusionSettings {
    pub apps: Vec<String>,
    pub sites: Vec<String>,
    pub titles: Vec<String>,
    pub defaults: Vec<DefaultItem>,
    pub always: Vec<&'static str>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DefaultItem {
    pub name: &'static str,
    /// "banking" or "health".
    pub category: &'static str,
    pub excluded: bool,
}

impl From<&Exclusions> for ExclusionSettings {
    fn from(exclusions: &Exclusions) -> Self {
        let turned_off = |name: &str| {
            exclusions
                .re_enabled_defaults
                .iter()
                .any(|off| off.trim().eq_ignore_ascii_case(name))
        };
        ExclusionSettings {
            apps: exclusions.apps.clone(),
            sites: exclusions.sites.clone(),
            titles: exclusions.title_patterns.clone(),
            defaults: DEFAULT_EXCLUSIONS
                .iter()
                .map(|rule| DefaultItem {
                    name: rule.name,
                    category: match rule.category {
                        Category::Banking => "banking",
                        Category::Health => "health",
                    },
                    excluded: !turned_off(rule.name),
                })
                .collect(),
            always: ALWAYS_EXCLUDED.to_vec(),
        }
    }
}

/// What the user typed, in the form it is stored and matched in.
fn normalize(kind: Kind, value: &str) -> Result<String, String> {
    let value = value.trim();
    match kind {
        Kind::App => {
            let valid = !value.is_empty()
                && value.chars().count() <= MAX_BUNDLE_ID_CHARS
                && value
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
            if valid {
                Ok(value.to_string())
            } else {
                Err("That doesn't look like an app's bundle id.".to_string())
            }
        }
        Kind::Site => {
            // Accept a whole address too, but keep only its host.
            let address = if value.contains("://") {
                value.to_string()
            } else {
                format!("https://{value}")
            };
            site_from_url(&address)
                .filter(|host| host.contains('.') || host == "localhost")
                .ok_or_else(|| "That doesn't look like a website, such as example.com.".to_string())
        }
        Kind::Title => {
            if value.is_empty() || value.chars().count() > MAX_TITLE_PATTERN_CHARS {
                Err(format!(
                    "Type some text from the window title (at most {MAX_TITLE_PATTERN_CHARS} characters)."
                ))
            } else {
                Ok(value.to_string())
            }
        }
    }
}

fn store_kind(kind: Kind) -> ExclusionKind {
    match kind {
        Kind::App => ExclusionKind::App,
        Kind::Site => ExclusionKind::Site,
        Kind::Title => ExclusionKind::Title,
    }
}

#[tauri::command(async)]
pub fn exclusion_settings(app: AppHandle) -> Result<ExclusionSettings, String> {
    change(&app, |_| Ok(()))
}

#[tauri::command(async)]
pub fn add_exclusion(
    app: AppHandle,
    kind: Kind,
    value: String,
) -> Result<ExclusionSettings, String> {
    let value = normalize(kind, &value)?;
    change(&app, |store| store.add_exclusion(store_kind(kind), &value))
}

#[tauri::command(async)]
pub fn remove_exclusion(
    app: AppHandle,
    kind: Kind,
    value: String,
) -> Result<ExclusionSettings, String> {
    change(&app, |store| {
        store.remove_exclusion(store_kind(kind), value.trim())
    })
}

/// Turns a banking or health default on (`excluded`) or off.
#[tauri::command(async)]
pub fn set_default_excluded(
    app: AppHandle,
    name: String,
    excluded: bool,
) -> Result<ExclusionSettings, String> {
    let rule = DEFAULT_EXCLUSIONS
        .iter()
        .find(|rule| rule.name.eq_ignore_ascii_case(name.trim()))
        .ok_or("There is no such default.")?;
    change(&app, |store| {
        if excluded {
            store.remove_exclusion(ExclusionKind::DefaultOff, rule.name)
        } else {
            store.add_exclusion(ExclusionKind::DefaultOff, rule.name)
        }
    })
}

/// Applies `update` to the store, reads the exclusions back and hands them
/// to capture. Capture only ever gets what was stored, so what the window
/// shows, what is stored and what capture uses can't drift apart.
fn change(
    app: &AppHandle,
    update: impl FnOnce(&Store) -> crate::store::Result<()>,
) -> Result<ExclusionSettings, String> {
    let store = app
        .try_state::<Mutex<Store>>()
        .ok_or("The database isn't available.")?;
    let store = store.lock().unwrap_or_else(PoisonError::into_inner);
    let exclusions = update(&store)
        .and_then(|()| store.exclusions())
        .map_err(|_| {
            logging::warn(
                "exclusions.change_failed",
                &[Field::Error(ErrorKind::Database)],
            );
            "Lax couldn't save your exclusions. Nothing was changed.".to_string()
        })?;
    let settings = ExclusionSettings::from(&exclusions);
    app.state::<Current>().set(exclusions);
    Ok(settings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn websites_keep_only_the_host() {
        assert_eq!(normalize(Kind::Site, "example.com").unwrap(), "example.com");
        assert_eq!(
            normalize(Kind::Site, " https://Chat.Example.com/room/42?x=1 ").unwrap(),
            "chat.example.com"
        );
        assert_eq!(normalize(Kind::Site, "localhost").unwrap(), "localhost");
        for nonsense in ["", "   ", "not a site", "nodot", "file:///etc/hosts"] {
            assert!(normalize(Kind::Site, nonsense).is_err(), "{nonsense:?}");
        }
    }

    #[test]
    fn apps_must_look_like_bundle_ids() {
        assert_eq!(
            normalize(Kind::App, " com.tinyspeck.slackmacgap ").unwrap(),
            "com.tinyspeck.slackmacgap"
        );
        for nonsense in ["", "Slack app", "com.example/../x", &"a".repeat(256)] {
            assert!(normalize(Kind::App, nonsense).is_err(), "{nonsense:?}");
        }
    }

    #[test]
    fn title_patterns_are_trimmed_and_bounded() {
        assert_eq!(
            normalize(Kind::Title, "  Private chat ").unwrap(),
            "Private chat"
        );
        assert!(normalize(Kind::Title, " ").is_err());
        assert!(normalize(Kind::Title, &"x".repeat(201)).is_err());
    }

    #[test]
    fn settings_list_every_default_and_the_always_excluded() {
        let mut exclusions = Exclusions::default();
        exclusions.re_enabled_defaults.insert("klarna".to_string());
        let settings = ExclusionSettings::from(&exclusions);
        assert_eq!(settings.defaults.len(), DEFAULT_EXCLUSIONS.len());
        let klarna = settings
            .defaults
            .iter()
            .find(|d| d.name == "Klarna")
            .unwrap();
        assert!(!klarna.excluded);
        let swedbank = settings
            .defaults
            .iter()
            .find(|d| d.name == "Swedbank")
            .unwrap();
        assert!(swedbank.excluded);
        assert_eq!(swedbank.category, "banking");
        assert!(
            settings
                .always
                .iter()
                .any(|a| a.contains("Password managers"))
        );
    }

    #[test]
    fn kinds_arrive_as_the_window_sends_them() {
        let kind: Kind = serde_json::from_str("\"site\"").unwrap();
        assert_eq!(kind, Kind::Site);
        // The window can't touch the tier-2 switches through add/remove.
        assert!(serde_json::from_str::<Kind>("\"defaultOff\"").is_err());
        assert!(serde_json::from_str::<Kind>("\"default_off\"").is_err());
    }
}
