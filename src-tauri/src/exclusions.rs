//! What Lax must not capture (GUARDRAILS §4).
//!
//! Exclusions come in three tiers:
//!
//! 1. **Never**, not configurable: password managers, system password prompts,
//!    private browser windows, Lax itself, and password fields anywhere.
//! 2. **Excluded by default**, the user can re-enable: banking, payments and
//!    health, matched on window title and site.
//! 3. **User-defined**: apps, sites and window title patterns.
//!
//! Excluded means not read. Capture asks in this order, and stops at the first
//! skip:
//!
//! 1. [`Exclusions::check_app`] with the bundle id, before reading anything
//!    from the app.
//! 2. [`Exclusions::check_window`] with the window title, before reading any
//!    content, and for browsers that have one, [`Exclusions::check_address_field`]
//!    with the description of the address field (never its value).
//! 3. [`Exclusions::check_site`] with the host of a browser window, before
//!    reading any content.
//! 4. [`is_secure_text_field`] for every element, so password fields are
//!    never read, even in apps that are otherwise captured.
//!
//! When in doubt, skip. Skips are counted per reason, never named.

use std::collections::HashSet;
use std::sync::{Mutex, PoisonError};

use chrono::NaiveDate;

/// Lax's own bundle identifier, from `tauri.conf.json`.
pub const LAX_BUNDLE_ID: &str = "io.github.fnyman-ctrl.lax";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SkipReason {
    /// Tier 1: never captured, whatever the settings say.
    Never,
    /// Tier 2: banking, payments or health, unless the user re-enabled it.
    ExcludedByDefault,
    /// Tier 3: excluded by the user.
    ExcludedByUser,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Capture,
    Skip(SkipReason),
}

/// Apps that are never captured. Each entry also matches its family: the
/// same id followed by `.something` (betas, helpers), ignoring case.
const NEVER_APPS: &[&str] = &[
    LAX_BUNDLE_ID,
    // Password managers
    "com.apple.keychainaccess",
    "com.apple.Passwords",
    "com.1password",
    "com.agilebits",
    "com.bitwarden.desktop",
    "org.keepassxc.keepassxc",
    "com.lastpass.LastPass",
    "com.markmcguill.strongbox",
    "com.hicknhacksoftware.MacPass",
    "in.sinew.Enpass-Desktop",
    "me.proton.pass.electron",
    // System password and login prompts
    "com.apple.SecurityAgent",
    "com.apple.loginwindow",
    // Every Tor Browser window is private.
    "org.torproject.torbrowser",
];

/// Browsers whose private windows and password pages must be skipped. Each
/// entry also matches its family (`com.google.Chrome.beta`,
/// `com.microsoft.edgemac.Dev`…).
const BROWSERS: &[&str] = &[
    "com.apple.Safari",
    "com.apple.SafariTechnologyPreview",
    "com.google.Chrome",
    "org.chromium.Chromium",
    "org.mozilla.firefox",
    "org.mozilla.firefoxdeveloperedition",
    "org.mozilla.nightly",
    "net.waterfox.waterfox",
    "io.gitlab.librewolf-community",
    "company.thebrowser.Browser",
    "com.microsoft.edgemac",
    "com.brave.Browser",
    "com.operasoftware.Opera",
    "com.operasoftware.OperaGX",
    "com.vivaldi.Vivaldi",
    "app.zen-browser.zen",
    "com.duckduckgo.macos.browser",
    "com.kagi.kagimacOS",
    "com.sigmaos.sigmaos.macos",
];

/// Window titles of password pages in browsers and System Settings: Chrome's
/// Password Manager, Safari's and Firefox's Passwords. Matched as whole words.
const PASSWORD_PAGE_WORDS: &[&str] = &[
    "password manager",
    "passwords",
    "lösenordshanteraren",
    "lösenordshanterare",
    "lösenord",
];

/// Apps besides browsers that can show saved passwords in a window.
const PASSWORD_PAGE_APPS: &[&str] = &["com.apple.systempreferences"];

/// Words that browsers put in the titles of private windows, in several
/// languages. Deliberately broad: a normal page titled "Private equity" is
/// skipped too, which is the safe mistake.
const PRIVATE_WINDOW_MARKERS: &[&str] = &[
    "incognito",
    "inkognito",
    "inprivate",
    "private",
    "privat",
    "privé",
    "privada",
    "privato",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Banking,
    Health,
}

/// A tier-2 exclusion: a service matched by name in window titles and by host.
#[derive(Debug)]
pub struct DefaultExclusion {
    /// Shown in settings, and the key used to re-enable it.
    pub name: &'static str,
    #[allow(dead_code)] // Groups the list in the settings window (#7).
    pub category: Category,
    /// Matched as whole words in window titles, ignoring case.
    title_words: &'static [&'static str],
    /// Matched as the host or any subdomain of it.
    hosts: &'static [&'static str],
}

pub const DEFAULT_EXCLUSIONS: &[DefaultExclusion] = &[
    bank("Swedbank", &["Swedbank"], &["swedbank.se", "swedbank.com"]),
    bank("SEB", &["SEB"], &["seb.se"]),
    bank("Handelsbanken", &["Handelsbanken"], &["handelsbanken.se"]),
    bank("Nordea", &["Nordea"], &["nordea.se", "nordea.com"]),
    bank(
        "Länsförsäkringar",
        &["Länsförsäkringar"],
        &["lansforsakringar.se"],
    ),
    bank("Danske Bank", &["Danske Bank"], &["danskebank.se"]),
    bank("ICA Banken", &["ICA Banken"], &["icabanken.se"]),
    bank("Skandia", &["Skandia", "Skandiabanken"], &["skandia.se"]),
    bank("Avanza", &["Avanza"], &["avanza.se"]),
    bank("Nordnet", &["Nordnet"], &["nordnet.se"]),
    bank("Klarna", &["Klarna"], &["klarna.com"]),
    bank("PayPal", &["PayPal"], &["paypal.com"]),
    bank("Revolut", &["Revolut"], &["revolut.com"]),
    // "Lunar" is also an ordinary English word, so a page about a lunar
    // eclipse is skipped too. That's the safe mistake.
    bank("Lunar", &["Lunar"], &["lunar.app"]),
    health("1177", &["1177", "1177 Vårdguiden"], &["1177.se"]),
    health("Kry", &["Kry"], &["kry.se"]),
    health("Min Doktor", &["Min Doktor"], &["mindoktor.se"]),
    health("Doktor.se", &["Doktor.se"], &["doktor.se"]),
    health("Apoteket", &["Apoteket"], &["apoteket.se"]),
    health("Apotek Hjärtat", &["Apotek Hjärtat"], &["apotekhjartat.se"]),
    health("Apotea", &["Apotea"], &["apotea.se"]),
];

const fn bank(
    name: &'static str,
    title_words: &'static [&'static str],
    hosts: &'static [&'static str],
) -> DefaultExclusion {
    DefaultExclusion {
        name,
        category: Category::Banking,
        title_words,
        hosts,
    }
}

const fn health(
    name: &'static str,
    title_words: &'static [&'static str],
    hosts: &'static [&'static str],
) -> DefaultExclusion {
    DefaultExclusion {
        name,
        category: Category::Health,
        title_words,
        hosts,
    }
}

/// The user's exclusion settings.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Exclusions {
    /// Bundle ids, matched exactly, ignoring case.
    pub apps: Vec<String>,
    /// Hosts, matched as the host or any subdomain of it.
    pub sites: Vec<String>,
    /// Matched anywhere in the window title, ignoring case.
    pub title_patterns: Vec<String>,
    /// Names of default exclusions the user has turned off.
    pub re_enabled_defaults: HashSet<String>,
}

impl Exclusions {
    /// Step 1: before reading anything from the app.
    pub fn check_app(&self, bundle_id: &str) -> Decision {
        let bundle_id = bundle_id.trim();
        // An app that can't be identified can't be checked, so skip it.
        if bundle_id.is_empty() || is_never_app(bundle_id) {
            return Decision::Skip(SkipReason::Never);
        }
        if self
            .apps
            .iter()
            .any(|app| app.trim().eq_ignore_ascii_case(bundle_id))
        {
            return Decision::Skip(SkipReason::ExcludedByUser);
        }
        Decision::Capture
    }

    /// Step 2: before reading any content of the window.
    pub fn check_window(&self, bundle_id: &str, title: &str) -> Decision {
        if let Decision::Skip(reason) = self.check_app(bundle_id) {
            return Decision::Skip(reason);
        }
        let words: Vec<char> = title.to_lowercase().chars().collect();
        let browser = is_browser(bundle_id);
        if browser && looks_private(&words) {
            return Decision::Skip(SkipReason::Never);
        }
        if (browser || matches_family(bundle_id, PASSWORD_PAGE_APPS))
            && PASSWORD_PAGE_WORDS
                .iter()
                .any(|w| has_word(&words, w, true))
        {
            return Decision::Skip(SkipReason::Never);
        }
        if self
            .active_defaults()
            .any(|rule| rule.title_words.iter().any(|w| has_word(&words, w, true)))
        {
            return Decision::Skip(SkipReason::ExcludedByDefault);
        }
        let title = title.to_lowercase();
        if self
            .title_patterns
            .iter()
            .map(|p| p.trim().to_lowercase())
            .any(|p| !p.is_empty() && title.contains(&p))
        {
            return Decision::Skip(SkipReason::ExcludedByUser);
        }
        Decision::Capture
    }

    /// Step 3: before reading any content of a browser window, with the host
    /// of its address.
    pub fn check_site(&self, host: &str) -> Decision {
        let host = host.trim().trim_end_matches('.').to_lowercase();
        if self
            .active_defaults()
            .any(|rule| rule.hosts.iter().any(|h| host_matches(&host, h)))
        {
            return Decision::Skip(SkipReason::ExcludedByDefault);
        }
        if self.sites.iter().any(|site| host_matches(&host, site)) {
            return Decision::Skip(SkipReason::ExcludedByUser);
        }
        Decision::Capture
    }

    fn active_defaults(&self) -> impl Iterator<Item = &'static DefaultExclusion> + '_ {
        DEFAULT_EXCLUSIONS.iter().filter(|rule| {
            !self
                .re_enabled_defaults
                .iter()
                .any(|name| name.trim().to_lowercase() == rule.name.to_lowercase())
        })
    }
}

/// Tier 1, as the window lists it: always excluded, not switchable.
pub const ALWAYS_EXCLUDED: &[&str] = &[
    "Password fields, in every app",
    "Private and incognito browser windows",
    "Password managers (1Password, Bitwarden, Keychain Access, Passwords and others)",
    "System password and login prompts",
    "Tor Browser",
    "Lax itself",
];

/// The exclusions capture uses, shared with the window, which changes them.
/// Capture picks up a change before its next read.
#[derive(Default)]
pub struct Current {
    /// A version that goes up with every change, and the exclusions.
    inner: Mutex<(u64, Exclusions)>,
}

impl Current {
    pub fn set(&self, exclusions: Exclusions) {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        inner.0 += 1;
        inner.1 = exclusions;
    }

    /// The exclusions, if they changed since version `seen`.
    pub fn newer_than(&self, seen: u64) -> Option<(u64, Exclusions)> {
        let inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        (inner.0 != seen).then(|| (inner.0, inner.1.clone()))
    }
}

/// Browsers whose address field says whether the window is private. Each
/// entry also matches its family. Safari's field is found by its
/// accessibility identifier, which is the same in every language, and its
/// description ends in "i ett privat fönster" / "in a private window" in
/// private windows.
const ADDRESS_FIELD_BROWSERS: &[(&str, &str)] = &[
    ("com.apple.Safari", "WEB_BROWSER_ADDRESS_AND_SEARCH_FIELD"),
    (
        "com.apple.SafariTechnologyPreview",
        "WEB_BROWSER_ADDRESS_AND_SEARCH_FIELD",
    ),
];

/// The accessibility identifier of the address field that tells whether a
/// window of this browser is private, if Lax knows one. For these browsers
/// capture must call [`Exclusions::check_address_field`] before reading any
/// content.
pub fn address_field_identifier(bundle_id: &str) -> Option<&'static str> {
    ADDRESS_FIELD_BROWSERS
        .iter()
        .find(|(browser, _)| matches_family(bundle_id, &[browser]))
        .map(|(_, identifier)| *identifier)
}

impl Exclusions {
    /// Step 2b, for browsers with an [`address_field_identifier`]: the
    /// description of that field, or `None` if it wasn't found. A window
    /// without the field can't be shown to be normal, so it is skipped
    /// (this also skips Safari web apps, which is the safe mistake).
    pub fn check_address_field(&self, bundle_id: &str, description: Option<&str>) -> Decision {
        if address_field_identifier(bundle_id).is_none() {
            return Decision::Capture;
        }
        match description {
            None => Decision::Skip(SkipReason::Never),
            Some(description) => {
                let words: Vec<char> = description.to_lowercase().chars().collect();
                if looks_private(&words) {
                    Decision::Skip(SkipReason::Never)
                } else {
                    Decision::Capture
                }
            }
        }
    }
}

/// Browsers keep a window's address in a field Lax never reads. Their page
/// content is read only from the web area, after its host was checked with
/// [`Exclusions::check_site`].
pub fn is_browser(bundle_id: &str) -> bool {
    matches_family(bundle_id, BROWSERS)
}

/// Step 4: password fields are never read, in any app.
pub fn is_secure_text_field(role: &str, subrole: Option<&str>) -> bool {
    const SECURE: &str = "AXSecureTextField";
    role == SECURE || subrole == Some(SECURE)
}

fn is_never_app(bundle_id: &str) -> bool {
    matches_family(bundle_id, NEVER_APPS)
}

/// `bundle_id` is one of `ids`, or in its family (`id.` followed by more),
/// ignoring case.
fn matches_family(bundle_id: &str, ids: &[&str]) -> bool {
    let bundle_id = bundle_id.trim();
    ids.iter().any(|id| {
        bundle_id
            .get(..id.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(id))
            && matches!(bundle_id.as_bytes().get(id.len()), None | Some(b'.'))
    })
}

/// A word in the (lower-cased) title starts with a private-window marker
/// ("privat" also matches "Privates Fenster" and "privata").
fn looks_private(words: &[char]) -> bool {
    PRIVATE_WINDOW_MARKERS
        .iter()
        .any(|marker| has_word(words, marker, false))
}

/// `host` is `domain` or a subdomain of it.
fn host_matches(host: &str, domain: &str) -> bool {
    let domain = domain.trim().trim_end_matches('.').to_lowercase();
    // "www.example.com" in a rule means the site, so it covers example.com
    // and all its subdomains.
    let domain = domain.strip_prefix("www.").unwrap_or(&domain);
    !domain.is_empty()
        && (host == domain
            || host
                .strip_suffix(domain)
                .is_some_and(|rest| rest.ends_with('.')))
}

/// `phrase` starts at a word boundary in `text`, which must already be lower
/// case. With `whole`, it must also end at one.
fn has_word(text: &[char], phrase: &str, whole: bool) -> bool {
    let phrase: Vec<char> = phrase.to_lowercase().chars().collect();
    if phrase.is_empty() || phrase.len() > text.len() {
        return false;
    }
    let is_word_char = |c: char| c.is_alphanumeric();
    (0..=text.len() - phrase.len()).any(|start| {
        let end = start + phrase.len();
        text[start..end] == phrase[..]
            && (start == 0 || !is_word_char(text[start - 1]))
            && (!whole || end == text.len() || !is_word_char(text[end]))
    })
}

/// How many windows were skipped today, per reason. Never which ones.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SkipCounts {
    pub never: u32,
    pub excluded_by_default: u32,
    pub excluded_by_user: u32,
}

#[derive(Debug, Default)]
pub struct SkipCounter {
    day: Option<NaiveDate>,
    counts: SkipCounts,
}

impl SkipCounter {
    pub fn record(&mut self, reason: SkipReason, today: NaiveDate) {
        self.roll_over(today);
        let count = match reason {
            SkipReason::Never => &mut self.counts.never,
            SkipReason::ExcludedByDefault => &mut self.counts.excluded_by_default,
            SkipReason::ExcludedByUser => &mut self.counts.excluded_by_user,
        };
        *count = count.saturating_add(1);
    }

    pub fn counts(&mut self, today: NaiveDate) -> SkipCounts {
        self.roll_over(today);
        self.counts
    }

    fn roll_over(&mut self, today: NaiveDate) {
        if self.day != Some(today) {
            self.day = Some(today);
            self.counts = SkipCounts::default();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SKIP_NEVER: Decision = Decision::Skip(SkipReason::Never);
    const SKIP_DEFAULT: Decision = Decision::Skip(SkipReason::ExcludedByDefault);
    const SKIP_USER: Decision = Decision::Skip(SkipReason::ExcludedByUser);

    fn none() -> Exclusions {
        Exclusions::default()
    }

    #[test]
    fn current_exclusions_report_each_change_once() {
        let current = Current::default();
        assert!(current.newer_than(0).is_none());
        current.set(Exclusions {
            apps: vec!["com.example.chat".to_string()],
            ..Exclusions::default()
        });
        let (version, exclusions) = current.newer_than(0).unwrap();
        assert_eq!(exclusions.apps, ["com.example.chat"]);
        assert!(current.newer_than(version).is_none());
    }

    #[test]
    fn ordinary_apps_are_captured() {
        assert_eq!(none().check_app("com.apple.mail"), Decision::Capture);
        assert_eq!(
            none().check_window("com.tinyspeck.slackmacgap", "Anna Example | Slack"),
            Decision::Capture
        );
    }

    #[test]
    fn password_managers_are_never_captured() {
        for app in [
            "com.apple.keychainaccess",
            "com.apple.Passwords",
            "com.1password.1password",
            "com.agilebits.onepassword7",
            "com.bitwarden.desktop",
            "org.keepassxc.keepassxc",
            "com.lastpass.LastPass",
        ] {
            assert_eq!(none().check_app(app), SKIP_NEVER, "{app}");
        }
    }

    #[test]
    fn more_password_managers_and_families() {
        for app in [
            "com.markmcguill.strongbox",
            "com.markmcguill.strongbox.mac",
            "com.hicknhacksoftware.MacPass",
            "in.sinew.Enpass-Desktop",
            "me.proton.pass.electron",
            "com.1password.1password-launcher",
            "org.torproject.torbrowser",
        ] {
            assert_eq!(none().check_app(app), SKIP_NEVER, "{app}");
        }
    }

    #[test]
    fn browser_password_pages_are_never_captured() {
        let cases = [
            (
                "com.google.Chrome",
                "Google Password Manager - Google Chrome",
            ),
            ("com.apple.Safari", "Passwords"),
            ("org.mozilla.firefox", "Passwords — Mozilla Firefox"),
            ("com.google.Chrome", "Lösenordshanteraren"),
            ("com.apple.systempreferences", "Passwords"),
        ];
        for (app, title) in cases {
            assert_eq!(none().check_window(app, title), SKIP_NEVER, "{title}");
        }
        // Outside browsers and System Settings it's just a word.
        assert_eq!(
            none().check_window("com.apple.mail", "Re: New passwords policy"),
            Decision::Capture
        );
    }

    #[test]
    fn private_windows_in_browser_betas_and_other_browsers() {
        let cases = [
            (
                "com.microsoft.edgemac.Beta",
                "Example - [InPrivate] - Microsoft Edge Beta",
            ),
            ("com.brave.Browser.nightly", "Example (Private)"),
            ("com.duckduckgo.macos.browser", "Private Browsing"),
            ("com.kagi.kagimacOS", "Private Browsing"),
        ];
        for (browser, title) in cases {
            assert_eq!(none().check_window(browser, title), SKIP_NEVER, "{browser}");
        }
    }

    #[test]
    fn system_password_prompts_are_never_captured() {
        assert_eq!(none().check_app("com.apple.SecurityAgent"), SKIP_NEVER);
        assert_eq!(none().check_app("com.apple.loginwindow"), SKIP_NEVER);
    }

    #[test]
    fn lax_never_captures_itself() {
        assert_eq!(none().check_app(LAX_BUNDLE_ID), SKIP_NEVER);
        let config = include_str!("../tauri.conf.json");
        assert!(
            config.contains(&format!(r#""identifier": "{LAX_BUNDLE_ID}""#)),
            "LAX_BUNDLE_ID must match the identifier in tauri.conf.json"
        );
    }

    #[test]
    fn never_tier_ignores_case_and_settings() {
        let mut settings = none();
        settings.apps.clear();
        assert_eq!(settings.check_app("COM.APPLE.PASSWORDS"), SKIP_NEVER);
    }

    #[test]
    fn unidentified_apps_are_skipped() {
        assert_eq!(none().check_app(""), SKIP_NEVER);
        assert_eq!(none().check_app("   "), SKIP_NEVER);
    }

    #[test]
    fn prefix_rules_do_not_match_lookalikes() {
        assert_eq!(
            none().check_app("com.1passwordfan.notes"),
            Decision::Capture
        );
    }

    #[test]
    fn private_browser_windows_are_never_captured() {
        let cases = [
            ("com.google.Chrome", "New Tab - Google Chrome (Incognito)"),
            (
                "org.mozilla.firefox",
                "Example — Mozilla Firefox Private Browsing",
            ),
            (
                "com.microsoft.edgemac",
                "Example - [InPrivate] - Microsoft Edge",
            ),
            ("com.apple.Safari", "Privat surfning"),
            ("company.thebrowser.Browser", "Incognito"),
            ("com.google.Chrome", "Neuer Tab - Google Chrome (Inkognito)"),
            ("org.mozilla.firefox", "Example — Navigation privée"),
        ];
        for (browser, title) in cases {
            assert_eq!(none().check_window(browser, title), SKIP_NEVER, "{title}");
        }
    }

    #[test]
    fn safari_private_windows_are_never_captured() {
        // As Safari 26 shows them in Swedish and English.
        assert_eq!(
            none().check_window("com.apple.Safari", "Aftonbladet, privat surfning"),
            SKIP_NEVER
        );
        assert_eq!(
            none().check_window("com.apple.Safari", "Example Domain, Private Browsing"),
            SKIP_NEVER
        );
        for description in [
            "smart sökfält i ett privat fönster",
            "smart search field in a private window",
        ] {
            assert_eq!(
                none().check_address_field("com.apple.Safari", Some(description)),
                SKIP_NEVER,
                "{description}"
            );
        }
    }

    #[test]
    fn safari_windows_without_an_address_field_are_skipped() {
        assert_eq!(
            none().check_address_field("com.apple.Safari", None),
            SKIP_NEVER
        );
        assert_eq!(
            none().check_address_field("com.apple.SafariTechnologyPreview", None),
            SKIP_NEVER
        );
    }

    #[test]
    fn normal_safari_windows_are_captured() {
        assert_eq!(
            none().check_window("com.apple.Safari", "Example Domain"),
            Decision::Capture
        );
        assert_eq!(
            none().check_address_field("com.apple.Safari", Some("smart sökfält")),
            Decision::Capture
        );
    }

    #[test]
    fn other_apps_need_no_address_field() {
        assert_eq!(address_field_identifier("com.google.Chrome"), None);
        assert_eq!(
            none().check_address_field("com.google.Chrome", None),
            Decision::Capture
        );
        assert_eq!(
            address_field_identifier("com.apple.Safari"),
            Some("WEB_BROWSER_ADDRESS_AND_SEARCH_FIELD")
        );
    }

    #[test]
    fn normal_browser_windows_are_captured() {
        assert_eq!(
            none().check_window("com.google.Chrome", "Q3 proposal - Google Docs"),
            Decision::Capture
        );
        // "Private" in a non-browser app is just a word.
        assert_eq!(
            none().check_window("com.apple.mail", "Re: Private equity meeting"),
            Decision::Capture
        );
    }

    #[test]
    fn banking_and_health_are_excluded_by_default() {
        let cases = [
            ("com.apple.Safari", "Internetbanken – Swedbank"),
            ("com.google.Chrome", "Logga in | SEB"),
            ("com.google.Chrome", "Mina sidor - Länsförsäkringar"),
            ("com.apple.Safari", "Inkorgen - 1177 Vårdguiden"),
            ("com.google.Chrome", "Betala med Klarna"),
            ("com.apple.Safari", "Lunar – Logga in"),
        ];
        for (app, title) in cases {
            assert_eq!(none().check_window(app, title), SKIP_DEFAULT, "{title}");
        }
        for host in [
            "swedbank.se",
            "online.swedbank.se",
            "e-tjanster.1177.se",
            "www.avanza.se",
            "www.lunar.app",
        ] {
            assert_eq!(none().check_site(host), SKIP_DEFAULT, "{host}");
        }
    }

    #[test]
    fn default_title_words_match_whole_words_only() {
        assert_eq!(
            none().check_window("com.apple.mail", "Sebastian Example: lunch?"),
            Decision::Capture
        );
        assert_eq!(
            none().check_window("com.apple.mail", "Nordeast region report"),
            Decision::Capture
        );
    }

    #[test]
    fn default_hosts_match_subdomains_not_lookalikes() {
        assert_eq!(none().check_site("notswedbank.se"), Decision::Capture);
        assert_eq!(
            none().check_site("swedbank.se.example.com"),
            Decision::Capture
        );
        assert_eq!(none().check_site("SWEDBANK.SE."), SKIP_DEFAULT);
    }

    #[test]
    fn user_can_re_enable_a_default_exclusion() {
        let mut settings = none();
        settings.re_enabled_defaults.insert("Avanza".to_string());
        assert_eq!(settings.check_site("www.avanza.se"), Decision::Capture);
        assert_eq!(
            settings.check_window("com.apple.Safari", "Avanza – Översikt"),
            Decision::Capture
        );
        // Other defaults stay excluded.
        assert_eq!(settings.check_site("nordnet.se"), SKIP_DEFAULT);
    }

    #[test]
    fn re_enabling_cannot_unlock_the_never_tier() {
        let mut settings = none();
        settings.re_enabled_defaults.insert("1Password".to_string());
        settings.re_enabled_defaults.insert("Incognito".to_string());
        assert_eq!(settings.check_app("com.1password.1password"), SKIP_NEVER);
        assert_eq!(
            settings.check_window("com.google.Chrome", "Example (Incognito)"),
            SKIP_NEVER
        );
    }

    #[test]
    fn user_defined_apps_sites_and_titles() {
        let settings = Exclusions {
            apps: vec!["com.facebook.archon".to_string()],
            sites: vec!["messenger.com".to_string()],
            title_patterns: vec!["Salary review".to_string()],
            ..Exclusions::default()
        };
        assert_eq!(settings.check_app("com.facebook.archon"), SKIP_USER);
        assert_eq!(
            settings.check_window("com.facebook.archon", "Chats"),
            SKIP_USER
        );
        assert_eq!(settings.check_site("www.messenger.com"), SKIP_USER);
        assert_eq!(
            settings.check_window("com.microsoft.Excel", "SALARY REVIEW 2026.xlsx"),
            SKIP_USER
        );
        assert_eq!(settings.check_app("com.apple.mail"), Decision::Capture);
    }

    #[test]
    fn user_sites_written_with_www_cover_the_whole_site() {
        let settings = Exclusions {
            sites: vec!["www.facebook.com".to_string()],
            ..Exclusions::default()
        };
        assert_eq!(settings.check_site("facebook.com"), SKIP_USER);
        assert_eq!(settings.check_site("www.facebook.com"), SKIP_USER);
        assert_eq!(settings.check_site("m.facebook.com"), SKIP_USER);
        assert_eq!(settings.check_site("notfacebook.com"), Decision::Capture);
    }

    #[test]
    fn re_enabling_ignores_case() {
        let mut settings = none();
        settings.re_enabled_defaults.insert("  avanza ".to_string());
        settings
            .re_enabled_defaults
            .insert("LÄNSFÖRSÄKRINGAR".to_string());
        assert_eq!(settings.check_site("avanza.se"), Decision::Capture);
        assert_eq!(
            settings.check_site("lansforsakringar.se"),
            Decision::Capture
        );
    }

    #[test]
    fn empty_user_patterns_match_nothing() {
        let settings = Exclusions {
            sites: vec![String::new()],
            title_patterns: vec!["  ".to_string()],
            ..Exclusions::default()
        };
        assert_eq!(settings.check_site("example.com"), Decision::Capture);
        assert_eq!(
            settings.check_window("com.apple.mail", "Inbox"),
            Decision::Capture
        );
    }

    #[test]
    fn password_fields_are_never_read() {
        assert!(is_secure_text_field(
            "AXTextField",
            Some("AXSecureTextField")
        ));
        assert!(is_secure_text_field("AXSecureTextField", None));
        assert!(!is_secure_text_field("AXTextField", None));
        assert!(!is_secure_text_field("AXTextArea", Some("AXSearchField")));
    }

    #[test]
    fn skips_are_counted_per_day_without_names() {
        let today = NaiveDate::from_ymd_opt(2026, 6, 15).unwrap();
        let tomorrow = today.succ_opt().unwrap();
        let mut counter = SkipCounter::default();
        counter.record(SkipReason::Never, today);
        counter.record(SkipReason::Never, today);
        counter.record(SkipReason::ExcludedByUser, today);
        assert_eq!(
            counter.counts(today),
            SkipCounts {
                never: 2,
                excluded_by_default: 0,
                excluded_by_user: 1
            }
        );
        assert_eq!(counter.counts(tomorrow), SkipCounts::default());
    }
}
