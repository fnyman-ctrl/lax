//! Capture: what's in the active window, read through the macOS Accessibility
//! API (GUARDRAILS §4).
//!
//! Every few seconds, [`Capturer::tick`] goes through these steps and stops at
//! the first one that says no:
//!
//! 1. **Pause.** While paused nothing is read, not even the app name. The
//!    pause is checked again before every later step, and before every element.
//! 2. **The app.** The frontmost app's bundle id, checked with
//!    [`Exclusions::check_app`].
//! 3. **The window.** The focused window's title, checked with
//!    [`Exclusions::check_window`]. For Safari also the description (never the
//!    value) of the address field, checked with
//!    [`Exclusions::check_address_field`].
//! 4. **The content.** The window is walked within [`Limits`]. Password fields
//!    are never read, and toolbars, which hold the address field, are skipped.
//!    A web area's host is checked with [`Exclusions::check_site`] before
//!    anything in it is read, and in browsers only web areas are read.
//! 5. **Unchanged?** Content identical to the last capture is skipped. The
//!    rest is handed to the store, which redacts it.
//!
//! The platform code is in `ax.rs`. This module only sees the [`Screen`] and
//! [`Node`] traits, so the order above is tested with synthetic windows.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::rc::Rc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};

use crate::exclusions::{
    Decision, Exclusions, SkipReason, address_field_identifier, is_browser, is_secure_text_field,
};
use crate::site::site_from_url;

/// The frontmost app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct App {
    pub pid: i32,
    /// Empty if the app has none; such apps are never captured.
    pub bundle_id: String,
    pub name: String,
}

/// Where capture reads from: the Accessibility API, or a synthetic screen in
/// tests.
pub trait Screen {
    type Node: Node;
    fn frontmost_app(&mut self) -> Option<App>;
    /// The focused window of `app`. Only called once the app was checked.
    fn focused_window(&mut self, app: &App) -> Option<Self::Node>;
}

/// An element in a window. Each method reads one attribute, and only when
/// called, so the order in [`Capturer::tick`] is the order of reading.
pub trait Node: Sized {
    fn role(&self) -> Option<String>;
    fn subrole(&self) -> Option<String>;
    /// Role and subrole together. The real screen reads both in one request,
    /// which the walk does for every element.
    fn role_and_subrole(&self) -> (Option<String>, Option<String>) {
        (self.role(), self.subrole())
    }
    fn identifier(&self) -> Option<String>;
    fn title(&self) -> Option<String>;
    fn description(&self) -> Option<String>;
    /// The address shown in a web area.
    fn url(&self) -> Option<String>;
    /// The visible text of an element with a text `role`, at most
    /// `max_chars` characters. This is content: it is only called once the
    /// element is known not to be a password field and everything around it
    /// was checked.
    fn text(&self, role: &str, max_chars: usize) -> Option<String>;
    /// At most `max` children.
    fn children(&self, max: usize) -> Vec<Self>;
}

/// Bounds on how much of a window is read, so a huge or hung app can't stall
/// capture or fill the database.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_depth: usize,
    pub max_nodes: usize,
    /// Children read per element.
    pub max_children: usize,
    /// Characters per capture.
    pub max_chars: usize,
    /// Characters per element, so one long document doesn't crowd out the
    /// rest of the window.
    pub max_element_chars: usize,
    /// Wall time for reading one window.
    pub time_budget: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_depth: 60,
            max_nodes: 3000,
            max_children: 400,
            max_chars: 20_000,
            max_element_chars: 5_000,
            time_budget: Duration::from_millis(1000),
        }
    }
}

/// How deep below the window to look for an address field. Safari's sits at
/// window → toolbar → group → field, and one level deeper in full screen
/// (window → group → toolbar → group → field). Two levels to spare.
const ADDRESS_FIELD_DEPTH: usize = 6;

/// Roles whose subtree is never read. Toolbars hold the address field, whose
/// value is the full address (GUARDRAILS §4 keeps only the host); menus are
/// app chrome.
const SKIPPED_ROLES: &[&str] = &["AXToolbar", "AXMenuBar", "AXMenu", "AXMenuItem"];

/// Parts of a web page that repeat on every page and say little about what
/// is on it: navigation menus, page headers and footers. Pages mark them as
/// landmarks, which browsers and Electron apps expose as these subroles of
/// a group. Skipped inside web areas, so captures start with the content.
const SKIPPED_WEB_LANDMARKS: &[&str] = &[
    "AXLandmarkNavigation",
    "AXLandmarkBanner",
    "AXLandmarkContentInfo",
];

/// Roles whose text is read.
const TEXT_ROLES: &[&str] = &["AXStaticText", "AXTextArea", "AXTextField", "AXComboBox"];

const WEB_AREA: &str = "AXWebArea";

/// What was read from a window, before redaction (the store redacts it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// When it was read, which can be up to [`MIN_GAP_PER_WINDOW`] before it
    /// is stored.
    pub captured_at: DateTime<Utc>,
    pub app_name: String,
    pub bundle_id: String,
    pub window_title: String,
    /// The host only.
    pub site: Option<String>,
    pub text: String,
    /// A limit was hit, so some of the window wasn't read.
    pub truncated: bool,
    pub nodes: usize,
    /// Frames inside the page whose host was excluded.
    pub skipped_frames: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Paused,
    NoApp,
    NoWindow,
    /// A window with neither a title nor text, such as the Finder desktop.
    Empty,
    Skipped(SkipReason),
    Unchanged,
    Captured(Snapshot),
}

pub struct Capturer {
    exclusions: Exclusions,
    limits: Limits,
    /// The fingerprint of the last stored snapshot.
    last: Option<u64>,
    throttle: Throttle,
}

impl Capturer {
    pub fn new(exclusions: Exclusions, limits: Limits) -> Self {
        Capturer {
            exclusions,
            limits,
            last: None,
            throttle: Throttle::new(MIN_GAP_PER_WINDOW),
        }
    }

    /// Reads the active window once. `paused` must return the current pause
    /// state; it is asked before every read.
    pub fn tick<S: Screen>(&mut self, screen: &mut S, paused: impl Fn() -> bool) -> Outcome {
        if paused() {
            return Outcome::Paused;
        }
        let Some(app) = screen.frontmost_app() else {
            return Outcome::NoApp;
        };
        if let Decision::Skip(reason) = self.exclusions.check_app(&app.bundle_id) {
            return Outcome::Skipped(reason);
        }

        if paused() {
            return Outcome::Paused;
        }
        let Some(window) = screen.focused_window(&app) else {
            return Outcome::NoWindow;
        };
        let raw_title = window.title().unwrap_or_default();
        if let Decision::Skip(reason) = self.exclusions.check_window(&app.bundle_id, &raw_title) {
            return Outcome::Skipped(reason);
        }
        if let Some(identifier) = address_field_identifier(&app.bundle_id) {
            if paused() {
                return Outcome::Paused;
            }
            let description = find_by_identifier(&window, identifier, &self.limits)
                .and_then(|field| field.description());
            if let Decision::Skip(reason) = self
                .exclusions
                .check_address_field(&app.bundle_id, description.as_deref())
            {
                return Outcome::Skipped(reason);
            }
        }

        let browser = is_browser(&app.bundle_id);
        let content = match walk(window, browser, &self.exclusions, &self.limits, &paused) {
            Ok(content) => content,
            Err(Stop::Paused) => return Outcome::Paused,
            Err(Stop::Skipped(reason)) => return Outcome::Skipped(reason),
        };

        let mut window_title = clean_title(&raw_title, &app.name, browser);
        if browser {
            window_title = title_without_address(&window_title);
        }
        let snapshot = Snapshot {
            captured_at: Utc::now(),
            window_title,
            app_name: app.name,
            bundle_id: app.bundle_id,
            site: content.site,
            text: content.text,
            truncated: content.truncated,
            nodes: content.nodes,
            skipped_frames: content.skipped_frames,
        };
        if snapshot.window_title.is_empty() && snapshot.text.is_empty() {
            return Outcome::Empty;
        }
        if self.last == Some(fingerprint(&snapshot)) {
            return Outcome::Unchanged;
        }
        Outcome::Captured(snapshot)
    }

    /// Takes over changed exclusions; they apply from the next tick. Content
    /// held back for storing is dropped, since it may come from what was
    /// just excluded.
    pub fn set_exclusions(&mut self, exclusions: Exclusions) {
        self.exclusions = exclusions;
        self.throttle.drop_held();
    }

    /// What to store after a tick with `outcome`, oldest first; see
    /// [`Throttle`]. Call [`Capturer::stored`] for each one that was stored.
    pub fn due(&mut self, outcome: Outcome, now: Instant) -> Vec<Snapshot> {
        self.throttle.after(outcome, now)
    }

    /// Tells the capturer that `snapshot` was stored, so the same content is
    /// skipped until it changes and the window waits for its next turn. A
    /// snapshot that was dropped (a pause began) or failed to store is not
    /// remembered, and is captured again.
    pub fn stored(&mut self, snapshot: &Snapshot, now: Instant) {
        self.last = Some(fingerprint(snapshot));
        self.throttle.stored(snapshot, now);
    }
}

/// How often a window whose text keeps changing is stored, at most.
pub const MIN_GAP_PER_WINDOW: Duration = Duration::from_secs(30);

/// Decides when captures are stored. A window whose text keeps changing (a
/// chat being answered, a document being typed) would otherwise be stored
/// every few seconds, nearly the same each time. It is stored at most once
/// per [`MIN_GAP_PER_WINDOW`], with its latest content. Switching to another
/// window stores what was held right away, so nothing is lost; a pause drops
/// it (GUARDRAILS §4: pausing stops any pending processing).
pub struct Throttle {
    min_gap: Duration,
    /// The window stored last, and when.
    last: Option<(WindowKey, Instant)>,
    /// The latest content of that window, read too soon to be stored.
    held: Option<Snapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WindowKey {
    bundle_id: String,
    window_title: String,
}

impl WindowKey {
    fn of(snapshot: &Snapshot) -> Self {
        WindowKey {
            bundle_id: snapshot.bundle_id.clone(),
            window_title: snapshot.window_title.clone(),
        }
    }
}

impl Throttle {
    pub fn new(min_gap: Duration) -> Self {
        Throttle {
            min_gap,
            last: None,
            held: None,
        }
    }

    /// What to store after a tick with `outcome`, oldest first. Call
    /// [`Throttle::stored`] for each one that was stored.
    pub fn after(&mut self, outcome: Outcome, now: Instant) -> Vec<Snapshot> {
        match outcome {
            Outcome::Paused => {
                self.held = None;
                Vec::new()
            }
            Outcome::Captured(snapshot) => {
                let key = WindowKey::of(&snapshot);
                let mut due = Vec::new();
                if let Some(held) = self.held.take()
                    && WindowKey::of(&held) != key
                {
                    due.push(held);
                }
                let recent = self.last.as_ref().is_some_and(|(last, at)| {
                    *last == key && now.duration_since(*at) < self.min_gap
                });
                // Held content is always from the window stored last, so after
                // a switch `recent` is false and the new window is stored too.
                if recent {
                    self.held = Some(snapshot);
                } else {
                    due.push(snapshot);
                }
                due
            }
            // Still the same window: store what was held once the gap is up.
            Outcome::Unchanged => {
                let gap_up = self
                    .last
                    .as_ref()
                    .is_none_or(|(_, at)| now.duration_since(*at) >= self.min_gap);
                if gap_up {
                    self.held.take().into_iter().collect()
                } else {
                    Vec::new()
                }
            }
            // Another window, or none: store what was held now.
            Outcome::NoApp | Outcome::NoWindow | Outcome::Empty | Outcome::Skipped(_) => {
                self.held.take().into_iter().collect()
            }
        }
    }

    pub fn stored(&mut self, snapshot: &Snapshot, now: Instant) {
        self.last = Some((WindowKey::of(snapshot), now));
    }

    fn drop_held(&mut self) {
        self.held = None;
    }
}

/// Unchanged content is recognised by this hash. It only lives in memory.
fn fingerprint(snapshot: &Snapshot) -> u64 {
    let mut hasher = DefaultHasher::new();
    (
        &snapshot.bundle_id,
        &snapshot.window_title,
        &snapshot.site,
        &snapshot.text,
    )
        .hash(&mut hasher);
    hasher.finish()
}

/// The first element below `root` with this accessibility identifier, not
/// looking inside web areas. Reads only roles and identifiers.
fn find_by_identifier<N: Node>(root: &N, identifier: &str, limits: &Limits) -> Option<N> {
    let mut level = root.children(limits.max_children);
    for _ in 0..ADDRESS_FIELD_DEPTH {
        let mut next = Vec::new();
        for node in level {
            if node.role().as_deref() == Some(WEB_AREA) {
                continue;
            }
            if node.identifier().as_deref() == Some(identifier) {
                return Some(node);
            }
            next.extend(node.children(limits.max_children));
        }
        level = next;
    }
    None
}

#[derive(Debug, PartialEq, Eq)]
enum Stop {
    Paused,
    Skipped(SkipReason),
}

#[derive(Debug, Default)]
struct Content {
    site: Option<String>,
    text: String,
    truncated: bool,
    nodes: usize,
    skipped_frames: usize,
}

/// Reads the text of `window`, depth first, in reading order.
fn walk<N: Node>(
    window: N,
    browser: bool,
    exclusions: &Exclusions,
    limits: &Limits,
    paused: &impl Fn() -> bool,
) -> Result<Content, Stop> {
    let deadline = Instant::now() + limits.time_budget;
    let mut content = Content::default();
    let mut lines: Vec<String> = Vec::new();
    let mut chars = 0;
    // (element, depth, inside a web area, the address of the web area if
    // the element is directly in one)
    let mut stack: Vec<(N, usize, bool, Option<Rc<str>>)> = vec![(window, 0, false, None)];

    while let Some((node, depth, in_web, page)) = stack.pop() {
        if paused() {
            return Err(Stop::Paused);
        }
        if content.nodes >= limits.max_nodes || Instant::now() >= deadline {
            content.truncated = true;
            break;
        }
        content.nodes += 1;

        // Neither is content: they say what kind of element this is.
        let (role, subrole) = node.role_and_subrole();
        let Some(role) = role else { continue };
        if SKIPPED_ROLES.contains(&role.as_str()) || is_secure_text_field(&role, subrole.as_deref())
        {
            continue;
        }
        if in_web
            && role == "AXGroup"
            && subrole
                .as_deref()
                .is_some_and(|subrole| SKIPPED_WEB_LANDMARKS.contains(&subrole))
        {
            continue;
        }

        if role == "AXLink"
            && let Some(page) = &page
            && node.url().is_some_and(|target| is_skip_link(page, &target))
        {
            continue;
        }

        let mut in_web = in_web;
        let mut page_address = None;
        if role == WEB_AREA {
            let address = node.url();
            page_address = address.as_deref().map(Rc::from);
            let host = address.as_deref().and_then(site_from_url);
            if let Some(host) = &host
                && let Decision::Skip(reason) = exclusions.check_site(host)
            {
                if in_web {
                    // A frame inside an allowed page: skip only the frame.
                    content.skipped_frames += 1;
                    continue;
                }
                return Err(Stop::Skipped(reason));
            }
            if browser && !in_web && address.is_none() {
                // A browser page whose address can't be read can't be
                // checked, so it is skipped.
                return Err(Stop::Skipped(SkipReason::Never));
            }
            if !in_web && content.site.is_none() {
                content.site = host;
            }
            in_web = true;
        } else if TEXT_ROLES.contains(&role.as_str()) {
            // In browsers only the page is read, never the browser's own
            // fields. Text elements are read whole, so their children are not.
            if browser && !in_web {
                continue;
            }
            let room = limits.max_chars.saturating_sub(chars);
            if room == 0 {
                content.truncated = true;
                break;
            }
            if let Some(text) = node.text(&role, room.min(limits.max_element_chars)) {
                let text = text.trim();
                if !text.is_empty() && lines.last().is_none_or(|last| last != text) {
                    // One more for the line break that joins it.
                    chars += text.chars().count() + 1;
                    lines.push(text.to_string());
                }
            }
            continue;
        }

        if depth >= limits.max_depth {
            content.truncated = true;
            continue;
        }
        let children = node.children(limits.max_children);
        stack.extend(
            children
                .into_iter()
                .rev()
                .map(|c| (c, depth + 1, in_web, page_address.clone())),
        );
    }

    let text = lines.join("\n");
    content.text = truncate_chars(&text, limits.max_chars);
    content.truncated |= content.text.len() < text.len();
    Ok(content)
}

/// A "skip to content" link: a link placed directly in a page that jumps to
/// another place on the same page. Pages put one first, for keyboard users,
/// so without this every capture would start with "Skip to content".
fn is_skip_link(page: &str, target: &str) -> bool {
    let without_fragment = |address: &str| address.split('#').next().unwrap_or("").to_string();
    let Some((_, fragment)) = target.split_once('#') else {
        return false;
    };
    // Single-page apps route with fragments ("#/inbox", "#!/settings"); those
    // links lead to other views, not further down the same page.
    let route = fragment.starts_with('/') || fragment.starts_with('!');
    !route && without_fragment(target) == without_fragment(page)
}

/// At most `max` characters of `text`, cut at a character boundary.
pub fn truncate_chars(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((end, _)) => text[..end].to_string(),
        None => text.to_string(),
    }
}

/// Browsers end window titles with their own name, and Chrome and Edge add
/// the profile name after it ("Q3 - Google Chrome - Anna"). Both are dropped.
fn clean_title(title: &str, app_name: &str, browser: bool) -> String {
    let title = title.trim();
    if !browser || app_name.trim().is_empty() {
        return title.to_string();
    }
    let names = [
        app_name.trim().to_string(),
        format!("Mozilla {}", app_name.trim()),
    ];
    for name in &names {
        for separator in [" - ", " — ", " – "] {
            if let Some(end) = title.rfind(&format!("{separator}{name}")) {
                return title[..end].trim_end().to_string();
            }
        }
    }
    title.to_string()
}

/// Browsers use the address as the title of a page without one
/// ("example.com/reset?token=…"). Only its host may be kept (GUARDRAILS §4),
/// so a title that reads as an address with a path, query or fragment is
/// replaced by its host.
fn title_without_address(title: &str) -> String {
    let looks_like_address = !title.is_empty()
        && !title.contains(char::is_whitespace)
        && title.contains(['/', '?', '#']);
    if !looks_like_address {
        return title.to_string();
    }
    let address = if title.contains("://") {
        title.to_string()
    } else {
        format!("https://{title}")
    };
    // Something that looks like an address but isn't one keeps nothing.
    site_from_url(&address).unwrap_or_default()
}

/// Without keyboard or mouse input for this long, the user counts as away.
pub const AWAY: Duration = Duration::from_secs(60);
const LONG_AWAY: Duration = Duration::from_secs(10 * 60);

/// How long to wait before the next read: the interval the user chose,
/// longer while they are away from the keyboard or on battery (#11).
///
/// While nobody types or moves the mouse, the screen rarely changes, and
/// what doesn't change isn't stored anyway, so reading less often loses
/// little. Video and slides changing on their own are read less often.
pub fn pace(interval: Duration, idle: Option<Duration>, on_battery: bool) -> Duration {
    const MAX: Duration = Duration::from_secs(300);
    let mut wait = match idle {
        Some(idle) if idle >= LONG_AWAY => interval.max(Duration::from_secs(60)),
        Some(idle) if idle >= AWAY => interval.max(Duration::from_secs(15)),
        _ => interval,
    };
    if on_battery {
        wait = wait.saturating_mul(2);
    }
    wait.min(MAX.max(interval))
}

/// When the capture loop last went round, so the window can tell whether
/// capture is actually running.
///
/// Measured on the monotonic clock, which, like the loop's sleep, stands
/// still while the Mac sleeps. On the wall clock, the loop would look dead
/// for the whole sleep right after waking up.
pub struct Heartbeat {
    start: Instant,
    /// Milliseconds after `start` of the last round, plus one; 0 = never.
    last: std::sync::atomic::AtomicU64,
    /// How long the loop said it would wait after that round, in ms.
    wait: std::sync::atomic::AtomicU64,
}

/// Room on top of the announced wait for one slow round.
const HEARTBEAT_SLACK: Duration = Duration::from_secs(15);

impl Default for Heartbeat {
    fn default() -> Self {
        Heartbeat {
            start: Instant::now(),
            last: std::sync::atomic::AtomicU64::new(0),
            wait: std::sync::atomic::AtomicU64::new(0),
        }
    }
}

impl Heartbeat {
    /// The loop went round and will wait about `next_wait` before the next.
    pub fn beat(&self, next_wait: Duration) {
        self.beat_at(self.start.elapsed(), next_wait);
    }

    /// Whether the loop went round when it said it would.
    pub fn alive(&self) -> bool {
        self.alive_at(self.start.elapsed())
    }

    fn beat_at(&self, since_start: Duration, next_wait: Duration) {
        let millis = |d: Duration| u64::try_from(d.as_millis()).unwrap_or(u64::MAX - 1);
        self.wait
            .store(millis(next_wait), std::sync::atomic::Ordering::Relaxed);
        self.last.store(
            millis(since_start) + 1,
            std::sync::atomic::Ordering::Relaxed,
        );
    }

    fn alive_at(&self, since_start: Duration) -> bool {
        match self.last.load(std::sync::atomic::Ordering::Relaxed) {
            0 => false,
            last => {
                let last = Duration::from_millis(last - 1);
                let wait =
                    Duration::from_millis(self.wait.load(std::sync::atomic::Ordering::Relaxed));
                since_start.saturating_sub(last) <= wait + HEARTBEAT_SLACK
            }
        }
    }
}

#[cfg(target_os = "macos")]
pub use run::start;

/// The capture loop of the running app.
#[cfg(target_os = "macos")]
mod run {
    use std::sync::{Mutex, PoisonError};
    use std::thread;
    use std::time::{Duration, Instant};

    use chrono::Local;
    use tauri::{AppHandle, Manager};

    use super::{Capturer, Heartbeat, Limits, Outcome, Snapshot};
    use crate::accessibility;
    use crate::ax::AxScreen;
    use crate::exclusions::{Current, Exclusions, SkipCounter};
    use crate::logging::{self, ErrorKind, Field};
    use crate::pause::Pause;
    use crate::store::{NewCapture, Store};

    /// How long another app may take to answer one Accessibility request.
    const MESSAGING_TIMEOUT: Duration = Duration::from_millis(250);

    /// How often a summary of what capture did (counts only) is logged.
    const SUMMARY_INTERVAL: Duration = Duration::from_secs(10 * 60);

    pub fn start(app: AppHandle) {
        thread::spawn(move || {
            crate::ax::set_messaging_timeout(MESSAGING_TIMEOUT);
            let mut screen = AxScreen::default();
            // User-defined exclusions arrive with the settings window.
            // The user's exclusions are loaded into `Current` before capture
            // starts, and picked up below before every tick.
            let mut capturer = Capturer::new(Exclusions::default(), Limits::default());
            let mut exclusions_seen = 0;
            let mut stats = Stats::default();
            let mut last_summary = Instant::now();
            loop {
                let started = Instant::now();
                let interval = interval(&app);
                app.state::<Heartbeat>().beat(interval);
                if accessibility::is_granted() {
                    let pause = app.state::<Pause>();
                    if let Some((version, exclusions)) =
                        app.state::<Current>().newer_than(exclusions_seen)
                    {
                        capturer.set_exclusions(exclusions);
                        exclusions_seen = version;
                    }
                    let mut outcome = capturer.tick(&mut screen, || pause.is_paused());
                    // Paused while reading: what was read is dropped.
                    if pause.is_paused() {
                        outcome = Outcome::Paused;
                    }
                    stats.record(&outcome, started.elapsed());
                    for snapshot in capturer.due(outcome, Instant::now()) {
                        let stored = store(&app, &snapshot);
                        if stored {
                            capturer.stored(&snapshot, Instant::now());
                        }
                        stats.record_store(stored);
                    }
                }
                if last_summary.elapsed() >= SUMMARY_INTERVAL {
                    stats.log();
                    stats = Stats {
                        skips: std::mem::take(&mut stats.skips),
                        ..Stats::default()
                    };
                    last_summary = Instant::now();
                }
                let idle = crate::ax::idle_time();
                let on_battery = crate::ax::on_battery().unwrap_or(false);
                stats.record_pace(idle, on_battery);
                let wait = super::pace(interval, idle, on_battery);
                app.state::<Heartbeat>().beat(wait);
                let away = idle.is_some_and(|idle| idle >= super::AWAY);
                sleep_until_due(wait, away);
            }
        });
    }

    /// Whether the store took care of `snapshot`: stored it, or refused it
    /// because the user forgot that time or app meanwhile. Either way it is
    /// remembered as handled, so content the user just forgot isn't stored
    /// again while it stays unchanged on screen.
    fn store(app: &AppHandle, snapshot: &Snapshot) -> bool {
        let Some(store) = app.try_state::<Mutex<Store>>() else {
            return false;
        };
        let store = store.lock().unwrap_or_else(PoisonError::into_inner);
        store
            .insert(&NewCapture {
                captured_at: snapshot.captured_at,
                app_name: &snapshot.app_name,
                bundle_id: &snapshot.bundle_id,
                window_title: &snapshot.window_title,
                site: snapshot.site.as_deref(),
                text: &snapshot.text,
            })
            .is_ok()
    }

    /// Sleeps `wait`, in steps of a second. After being away, wakes up as
    /// soon as the user is back, so their first minute back is read.
    fn sleep_until_due(wait: Duration, away: bool) {
        let start = Instant::now();
        while start.elapsed() < wait {
            thread::sleep((wait - start.elapsed()).min(Duration::from_secs(1)));
            if away && crate::ax::idle_time().is_some_and(|idle| idle < super::AWAY) {
                return;
            }
        }
    }

    fn interval(app: &AppHandle) -> Duration {
        let seconds = app
            .try_state::<Mutex<Store>>()
            .and_then(|store| {
                let store = store.lock().unwrap_or_else(PoisonError::into_inner);
                store.capture_interval_seconds().ok()
            })
            .unwrap_or(crate::store::DEFAULT_CAPTURE_INTERVAL_SECONDS);
        Duration::from_secs(u64::from(seconds))
    }

    /// Counts and timings since the last summary. Never what was captured.
    #[derive(Default)]
    struct Stats {
        ticks: u64,
        changed: u64,
        stored: u64,
        unchanged: u64,
        paused: u64,
        no_window: u64,
        truncated: u64,
        skipped_frames: u64,
        store_errors: u64,
        largest_window: u64,
        /// Rounds while the user was away from the keyboard (a minute or
        /// more), and while on battery.
        away: u64,
        battery: u64,
        slowest: Duration,
        skips: SkipCounter,
    }

    impl Stats {
        fn record(&mut self, outcome: &Outcome, elapsed: Duration) {
            self.ticks += 1;
            self.slowest = self.slowest.max(elapsed);
            match outcome {
                Outcome::Paused => self.paused += 1,
                Outcome::NoApp | Outcome::NoWindow | Outcome::Empty => self.no_window += 1,
                Outcome::Skipped(reason) => self.skips.record(*reason, Local::now().date_naive()),
                Outcome::Unchanged => self.unchanged += 1,
                Outcome::Captured(snapshot) => {
                    self.changed += 1;
                    self.truncated += u64::from(snapshot.truncated);
                    self.skipped_frames += snapshot.skipped_frames as u64;
                    self.largest_window = self.largest_window.max(snapshot.nodes as u64);
                }
            }
        }

        fn record_pace(&mut self, idle: Option<Duration>, on_battery: bool) {
            if idle.is_some_and(|idle| idle >= Duration::from_secs(60)) {
                self.away += 1;
            }
            if on_battery {
                self.battery += 1;
            }
        }

        fn record_store(&mut self, stored: bool) {
            if stored {
                self.stored += 1;
            } else {
                self.store_errors += 1;
            }
        }

        fn log(&mut self) {
            let skips = self.skips.counts(Local::now().date_naive());
            logging::info(
                "capture.summary",
                &[
                    Field::Count("ticks", self.ticks),
                    Field::Count("changed", self.changed),
                    Field::Count("stored", self.stored),
                    Field::Count("unchanged", self.unchanged),
                    Field::Count("paused", self.paused),
                    Field::Count("no_window", self.no_window),
                    Field::Count("truncated", self.truncated),
                    Field::Count("skipped_frames", self.skipped_frames),
                    Field::Count("skipped_today_never", u64::from(skips.never)),
                    Field::Count(
                        "skipped_today_default",
                        u64::from(skips.excluded_by_default),
                    ),
                    Field::Count("skipped_today_user", u64::from(skips.excluded_by_user)),
                    Field::Count("largest_window_elements", self.largest_window),
                    Field::Count("away", self.away),
                    Field::Count("battery", self.battery),
                    Field::Duration("slowest", self.slowest),
                ],
            );
            if self.store_errors > 0 {
                logging::warn(
                    "capture.store_failed",
                    &[
                        Field::Count("failures", self.store_errors),
                        Field::Error(ErrorKind::Database),
                    ],
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// Every attribute read, in order, as "attribute:element".
    type Reads = Rc<RefCell<Vec<String>>>;

    #[derive(Clone, Default)]
    struct Fake {
        name: &'static str,
        role: &'static str,
        subrole: Option<&'static str>,
        identifier: Option<&'static str>,
        title: Option<&'static str>,
        description: Option<&'static str>,
        url: Option<&'static str>,
        text: Option<String>,
        children: Vec<Fake>,
    }

    struct FakeNode {
        fake: Fake,
        reads: Reads,
    }

    impl FakeNode {
        fn read(&self, attribute: &str) {
            self.reads
                .borrow_mut()
                .push(format!("{attribute}:{}", self.fake.name));
        }
    }

    impl Node for FakeNode {
        fn role(&self) -> Option<String> {
            self.read("role");
            Some(self.fake.role.to_string())
        }
        fn subrole(&self) -> Option<String> {
            self.read("subrole");
            self.fake.subrole.map(String::from)
        }
        fn identifier(&self) -> Option<String> {
            self.read("identifier");
            self.fake.identifier.map(String::from)
        }
        fn title(&self) -> Option<String> {
            self.read("title");
            self.fake.title.map(String::from)
        }
        fn description(&self) -> Option<String> {
            self.read("description");
            self.fake.description.map(String::from)
        }
        fn url(&self) -> Option<String> {
            self.read("url");
            self.fake.url.map(String::from)
        }
        fn text(&self, _role: &str, max_chars: usize) -> Option<String> {
            self.read("text");
            self.fake
                .text
                .as_deref()
                .map(|t| truncate_chars(t, max_chars))
        }
        fn children(&self, max: usize) -> Vec<Self> {
            self.read("children");
            self.fake
                .children
                .iter()
                .take(max)
                .map(|c| FakeNode {
                    fake: c.clone(),
                    reads: self.reads.clone(),
                })
                .collect()
        }
    }

    struct FakeScreen {
        app: Option<App>,
        window: Option<Fake>,
        reads: Reads,
    }

    impl FakeScreen {
        fn new(bundle_id: &str, name: &str, window: Fake) -> Self {
            FakeScreen {
                app: Some(App {
                    pid: 42,
                    bundle_id: bundle_id.to_string(),
                    name: name.to_string(),
                }),
                window: Some(window),
                reads: Reads::default(),
            }
        }

        fn reads(&self) -> Vec<String> {
            self.reads.borrow().clone()
        }

        /// Whether any content (text) was read at all.
        fn read_text(&self) -> bool {
            self.reads().iter().any(|r| r.starts_with("text:"))
        }
    }

    impl Screen for FakeScreen {
        type Node = FakeNode;
        fn frontmost_app(&mut self) -> Option<App> {
            self.reads.borrow_mut().push("app".to_string());
            self.app.clone()
        }
        fn focused_window(&mut self, _app: &App) -> Option<FakeNode> {
            self.reads.borrow_mut().push("window".to_string());
            self.window.clone().map(|fake| FakeNode {
                fake,
                reads: self.reads.clone(),
            })
        }
    }

    fn el(name: &'static str, role: &'static str) -> Fake {
        Fake {
            name,
            role,
            ..Fake::default()
        }
    }

    fn text(name: &'static str, value: &str) -> Fake {
        Fake {
            text: Some(value.to_string()),
            ..el(name, "AXStaticText")
        }
    }

    fn window(title: &'static str, children: Vec<Fake>) -> Fake {
        Fake {
            title: Some(title),
            children,
            ..el("window", "AXWindow")
        }
    }

    fn web_area(url: &'static str, children: Vec<Fake>) -> Fake {
        Fake {
            url: Some(url),
            children,
            ..el("page", WEB_AREA)
        }
    }

    fn safari_toolbar(description: &'static str) -> Fake {
        Fake {
            children: vec![Fake {
                children: vec![Fake {
                    identifier: Some("WEB_BROWSER_ADDRESS_AND_SEARCH_FIELD"),
                    description: Some(description),
                    text: Some("https://www.example.com/secret-path?q=anna".to_string()),
                    ..el("address", "AXTextField")
                }],
                ..el("group", "AXGroup")
            }],
            ..el("toolbar", "AXToolbar")
        }
    }

    fn capturer() -> Capturer {
        Capturer::new(Exclusions::default(), Limits::default())
    }

    fn running() -> bool {
        false
    }

    fn captured(outcome: Outcome) -> Snapshot {
        match outcome {
            Outcome::Captured(snapshot) => snapshot,
            other => panic!("expected a capture, got {other:?}"),
        }
    }

    fn notes() -> FakeScreen {
        FakeScreen::new(
            "com.apple.Notes",
            "Notes",
            window(
                "Q3 proposal",
                vec![
                    text("a", "Meeting with Anna Example"),
                    text("b", "Deliver by Friday"),
                ],
            ),
        )
    }

    #[test]
    fn captures_app_title_and_text() {
        let mut screen = notes();
        let snapshot = captured(capturer().tick(&mut screen, running));
        assert_eq!(snapshot.app_name, "Notes");
        assert_eq!(snapshot.bundle_id, "com.apple.Notes");
        assert_eq!(snapshot.window_title, "Q3 proposal");
        assert_eq!(
            snapshot.text,
            "Meeting with Anna Example\nDeliver by Friday"
        );
        assert_eq!(snapshot.site, None);
        assert!(!snapshot.truncated);
    }

    #[test]
    fn while_paused_nothing_is_read() {
        let mut screen = notes();
        assert_eq!(capturer().tick(&mut screen, || true), Outcome::Paused);
        assert!(screen.reads().is_empty(), "{:?}", screen.reads());
    }

    #[test]
    fn a_pause_during_reading_stops_it() {
        let mut screen = notes();
        let calls = std::cell::Cell::new(0);
        // Running for the first few checks, then paused.
        let paused = || {
            calls.set(calls.get() + 1);
            calls.get() > 3
        };
        assert_eq!(capturer().tick(&mut screen, paused), Outcome::Paused);
        assert!(!screen.read_text());
    }

    #[test]
    fn password_managers_are_not_read_at_all() {
        let mut screen = FakeScreen::new(
            "com.1password.1password",
            "1Password",
            window("Vault", vec![text("a", "hunter2")]),
        );
        assert_eq!(
            capturer().tick(&mut screen, running),
            Outcome::Skipped(SkipReason::Never)
        );
        assert_eq!(screen.reads(), vec!["app"]);
    }

    #[test]
    fn apps_without_a_bundle_id_are_not_read() {
        let mut screen = FakeScreen::new("", "Unknown", window("x", vec![text("a", "y")]));
        assert_eq!(
            capturer().tick(&mut screen, running),
            Outcome::Skipped(SkipReason::Never)
        );
        assert_eq!(screen.reads(), vec!["app"]);
    }

    #[test]
    fn exclusions_are_checked_before_content_is_read() {
        // Excluded by title (tier 2): only the title was read.
        let mut screen = FakeScreen::new(
            "com.apple.mail",
            "Mail",
            window("Ditt kontoutdrag från Swedbank", vec![text("a", "Saldo")]),
        );
        assert_eq!(
            capturer().tick(&mut screen, running),
            Outcome::Skipped(SkipReason::ExcludedByDefault)
        );
        assert_eq!(screen.reads(), vec!["app", "window", "title:window"]);

        // Excluded by the user.
        let exclusions = Exclusions {
            title_patterns: vec!["private chat".to_string()],
            ..Exclusions::default()
        };
        let mut screen = FakeScreen::new(
            "com.tinyspeck.slackmacgap",
            "Slack",
            window("Private chat with Anna", vec![text("a", "hi")]),
        );
        let mut capturer = Capturer::new(exclusions, Limits::default());
        assert_eq!(
            capturer.tick(&mut screen, running),
            Outcome::Skipped(SkipReason::ExcludedByUser)
        );
        assert!(!screen.read_text());
    }

    #[test]
    fn excluded_sites_are_not_read() {
        let mut screen = FakeScreen::new(
            "com.google.Chrome",
            "Google Chrome",
            window(
                "Logga in - Google Chrome",
                vec![web_area(
                    "https://online.swedbank.se/app/login",
                    vec![text("a", "Saldo 1 234 kr")],
                )],
            ),
        );
        assert_eq!(
            capturer().tick(&mut screen, running),
            Outcome::Skipped(SkipReason::ExcludedByDefault)
        );
        assert!(!screen.read_text());
    }

    #[test]
    fn excluded_frames_inside_a_page_are_not_read() {
        let mut screen = FakeScreen::new(
            "com.google.Chrome",
            "Google Chrome",
            window(
                "Shop - Google Chrome",
                vec![web_area(
                    "https://shop.example.com/checkout",
                    vec![
                        text("a", "Your order"),
                        Fake {
                            name: "frame",
                            ..web_area(
                                "https://checkout.klarna.com/pay",
                                vec![text("b", "Card details")],
                            )
                        },
                    ],
                )],
            ),
        );
        let snapshot = captured(capturer().tick(&mut screen, running));
        assert_eq!(snapshot.text, "Your order");
        assert_eq!(snapshot.skipped_frames, 1);
        assert!(!screen.reads().contains(&"text:b".to_string()));
    }

    #[test]
    fn private_browser_windows_are_not_read() {
        let mut screen = FakeScreen::new(
            "com.google.Chrome",
            "Google Chrome",
            window(
                "New Tab - Google Chrome (Incognito)",
                vec![web_area("https://example.com/", vec![text("a", "x")])],
            ),
        );
        assert_eq!(
            capturer().tick(&mut screen, running),
            Outcome::Skipped(SkipReason::Never)
        );
        assert_eq!(screen.reads(), vec!["app", "window", "title:window"]);
    }

    #[test]
    fn safari_private_windows_are_not_read() {
        // Even if the title had no marker, the address field says private.
        let mut screen = FakeScreen::new(
            "com.apple.Safari",
            "Safari",
            window(
                "Example Domain",
                vec![
                    web_area("https://example.com/", vec![text("a", "Example Domain")]),
                    safari_toolbar("smart sökfält i ett privat fönster"),
                ],
            ),
        );
        assert_eq!(
            capturer().tick(&mut screen, running),
            Outcome::Skipped(SkipReason::Never)
        );
        assert!(!screen.read_text());
        // The address field's description was read, never its value.
        assert!(screen.reads().contains(&"description:address".to_string()));
        assert!(!screen.reads().contains(&"text:address".to_string()));
    }

    #[test]
    fn safari_full_screen_windows_are_read() {
        // In full screen the toolbar sits inside an extra group.
        let mut screen = FakeScreen::new(
            "com.apple.Safari",
            "Safari",
            window(
                "Example Domain",
                vec![
                    web_area("https://example.com/", vec![text("a", "Example Domain")]),
                    Fake {
                        children: vec![safari_toolbar("smart sökfält")],
                        ..el("full-screen-group", "AXGroup")
                    },
                ],
            ),
        );
        let snapshot = captured(capturer().tick(&mut screen, running));
        assert_eq!(snapshot.text, "Example Domain");

        let mut private = FakeScreen::new(
            "com.apple.Safari",
            "Safari",
            window(
                "Example Domain",
                vec![Fake {
                    children: vec![safari_toolbar("smart sökfält i ett privat fönster")],
                    ..el("full-screen-group", "AXGroup")
                }],
            ),
        );
        assert_eq!(
            capturer().tick(&mut private, running),
            Outcome::Skipped(SkipReason::Never)
        );
    }

    #[test]
    fn safari_windows_without_an_address_field_are_not_read() {
        let mut screen = FakeScreen::new(
            "com.apple.Safari",
            "Safari",
            window(
                "Example Domain",
                vec![web_area("https://example.com/", vec![text("a", "x")])],
            ),
        );
        assert_eq!(
            capturer().tick(&mut screen, running),
            Outcome::Skipped(SkipReason::Never)
        );
        assert!(!screen.read_text());
    }

    #[test]
    fn normal_safari_windows_keep_only_the_host_and_the_page() {
        let mut screen = FakeScreen::new(
            "com.apple.Safari",
            "Safari",
            window(
                "Example Domain",
                vec![
                    web_area(
                        "https://www.example.com/docs/q3?token=abc",
                        vec![text("a", "Example Domain")],
                    ),
                    safari_toolbar("smart sökfält"),
                ],
            ),
        );
        let snapshot = captured(capturer().tick(&mut screen, running));
        assert_eq!(snapshot.site.as_deref(), Some("www.example.com"));
        assert_eq!(snapshot.text, "Example Domain");
        assert!(!snapshot.text.contains("secret-path"));
        assert!(!screen.reads().contains(&"text:address".to_string()));
    }

    #[test]
    fn browser_pages_whose_address_cannot_be_read_are_skipped() {
        let mut screen = FakeScreen::new(
            "com.google.Chrome",
            "Google Chrome",
            window(
                "Example - Google Chrome",
                vec![Fake {
                    children: vec![text("a", "x")],
                    ..el("page", WEB_AREA)
                }],
            ),
        );
        assert_eq!(
            capturer().tick(&mut screen, running),
            Outcome::Skipped(SkipReason::Never)
        );
        assert!(!screen.read_text());
    }

    #[test]
    fn browsers_read_only_the_page() {
        let mut screen = FakeScreen::new(
            "com.google.Chrome",
            "Google Chrome",
            window(
                "Example - Google Chrome",
                vec![
                    Fake {
                        text: Some("https://example.com/full/path".to_string()),
                        ..el("omnibox", "AXTextField")
                    },
                    web_area("https://example.com/full/path", vec![text("a", "Hello")]),
                ],
            ),
        );
        let snapshot = captured(capturer().tick(&mut screen, running));
        assert_eq!(snapshot.text, "Hello");
        assert_eq!(snapshot.site.as_deref(), Some("example.com"));
        assert!(!screen.reads().contains(&"text:omnibox".to_string()));
    }

    #[test]
    fn password_fields_are_never_read() {
        let mut screen = FakeScreen::new(
            "com.example.app",
            "Example",
            window(
                "Sign in",
                vec![
                    text("label", "Password"),
                    Fake {
                        subrole: Some("AXSecureTextField"),
                        text: Some("hunter2".to_string()),
                        ..el("by-subrole", "AXTextField")
                    },
                    Fake {
                        text: Some("hunter2".to_string()),
                        children: vec![text("inside", "hunter2")],
                        ..el("by-role", "AXSecureTextField")
                    },
                    Fake {
                        text: Some("anna@example.com".to_string()),
                        ..el("email", "AXTextField")
                    },
                ],
            ),
        );
        let snapshot = captured(capturer().tick(&mut screen, running));
        assert_eq!(snapshot.text, "Password\nanna@example.com");
        let reads = screen.reads();
        for name in ["by-subrole", "by-role", "inside"] {
            assert!(!reads.contains(&format!("text:{name}")), "{name}");
        }
    }

    #[test]
    fn page_navigation_headers_and_footers_are_skipped() {
        let landmark = |name, subrole, text_name, value| Fake {
            subrole: Some(subrole),
            children: vec![text(text_name, value)],
            ..el(name, "AXGroup")
        };
        let mut screen = FakeScreen::new(
            "com.google.Chrome",
            "Google Chrome",
            window(
                "Q3 proposal - Google Chrome",
                vec![web_area(
                    "https://docs.example.com/q3",
                    vec![
                        landmark(
                            "nav",
                            "AXLandmarkNavigation",
                            "menu",
                            "Skip to content Home Issues",
                        ),
                        landmark("header", "AXLandmarkBanner", "logo", "Example Docs"),
                        landmark("main", "AXLandmarkMain", "body", "Deliver by Friday"),
                        landmark("footer", "AXLandmarkContentInfo", "legal", "© Example"),
                    ],
                )],
            ),
        );
        let snapshot = captured(capturer().tick(&mut screen, running));
        assert_eq!(snapshot.text, "Deliver by Friday");
    }

    #[test]
    fn skip_to_content_links_are_skipped() {
        let link = |name, url, label| Fake {
            url: Some(url),
            children: vec![text(label, label)],
            ..el(name, "AXLink")
        };
        let mut screen = FakeScreen::new(
            "com.apple.Safari",
            "Safari",
            window(
                "Q3",
                vec![
                    web_area(
                        "https://docs.example.com/q3",
                        vec![
                            link(
                                "skip",
                                "https://docs.example.com/q3#start-of-content",
                                "Skip to content",
                            ),
                            link("other", "https://docs.example.com/q4", "Q4 report"),
                            Fake {
                                children: vec![link(
                                    "toc",
                                    "https://docs.example.com/q3#summary",
                                    "Summary",
                                )],
                                ..el("main", "AXGroup")
                            },
                        ],
                    ),
                    safari_toolbar("smart sökfält"),
                ],
            ),
        );
        let snapshot = captured(capturer().tick(&mut screen, running));
        // Links elsewhere, and in-page links deeper in the page, are kept.
        assert_eq!(snapshot.text, "Q4 report\nSummary");
        assert!(is_skip_link(
            "https://a.example/x",
            "https://a.example/x#main"
        ));
        assert!(!is_skip_link(
            "https://a.example/x",
            "https://a.example/y#main"
        ));
        assert!(!is_skip_link("https://a.example/x", "https://a.example/x"));
        // Routes in single-page apps are not skip links.
        assert!(!is_skip_link(
            "https://a.example/#/inbox",
            "https://a.example/#/sent"
        ));
        assert!(!is_skip_link(
            "https://a.example/#!/a",
            "https://a.example/#!/b"
        ));
    }

    #[test]
    fn landmarks_outside_web_pages_are_read() {
        // Native apps don't use web landmarks; a group is just a group.
        let mut screen = FakeScreen::new(
            "com.example.app",
            "Example",
            window(
                "Doc",
                vec![Fake {
                    subrole: Some("AXLandmarkNavigation"),
                    children: vec![text("a", "Sidebar note")],
                    ..el("group", "AXGroup")
                }],
            ),
        );
        let snapshot = captured(capturer().tick(&mut screen, running));
        assert_eq!(snapshot.text, "Sidebar note");
    }

    #[test]
    fn toolbars_and_menus_are_not_read() {
        let mut screen = FakeScreen::new(
            "com.example.app",
            "Example",
            window(
                "Doc",
                vec![
                    Fake {
                        children: vec![text("search", "my search")],
                        ..el("toolbar", "AXToolbar")
                    },
                    Fake {
                        children: vec![text("item", "Recent: secret.pdf")],
                        ..el("menu", "AXMenu")
                    },
                    text("body", "Body"),
                ],
            ),
        );
        let snapshot = captured(capturer().tick(&mut screen, running));
        assert_eq!(snapshot.text, "Body");
    }

    fn deep(depth: usize) -> Fake {
        let mut node = text("leaf", "deep text");
        for _ in 0..depth {
            node = Fake {
                children: vec![node],
                ..el("group", "AXGroup")
            };
        }
        node
    }

    #[test]
    fn depth_is_limited() {
        let limits = Limits {
            max_depth: 5,
            ..Limits::default()
        };
        let mut screen = FakeScreen::new("com.example.app", "Example", window("x", vec![deep(10)]));
        let snapshot =
            captured(Capturer::new(Exclusions::default(), limits).tick(&mut screen, running));
        assert!(snapshot.truncated);
        assert_eq!(snapshot.text, "");
    }

    #[test]
    fn node_count_is_limited() {
        let limits = Limits {
            max_nodes: 10,
            ..Limits::default()
        };
        let many: Vec<Fake> = (0..100).map(|_| text("t", "line")).collect();
        let mut screen = FakeScreen::new("com.example.app", "Example", window("x", many));
        let snapshot =
            captured(Capturer::new(Exclusions::default(), limits).tick(&mut screen, running));
        assert!(snapshot.truncated);
        assert_eq!(snapshot.nodes, 10);
    }

    #[test]
    fn text_length_is_limited() {
        let limits = Limits {
            max_chars: 30,
            max_element_chars: 20,
            ..Limits::default()
        };
        let long = "å".repeat(100);
        let long = long.as_str();
        let mut screen = FakeScreen::new(
            "com.example.app",
            "Example",
            window("x", vec![text("a", long), text("b", long), text("c", long)]),
        );
        let snapshot =
            captured(Capturer::new(Exclusions::default(), limits).tick(&mut screen, running));
        assert_eq!(snapshot.text.chars().count(), 30);
        assert!(snapshot.truncated);
        assert!(!screen.reads().contains(&"text:c".to_string()));
    }

    #[test]
    fn children_per_element_are_limited() {
        let limits = Limits {
            max_children: 3,
            ..Limits::default()
        };
        let many: Vec<Fake> = (0..10).map(|_| text("t", "line")).collect();
        let mut screen = FakeScreen::new("com.example.app", "Example", window("x", many));
        let snapshot =
            captured(Capturer::new(Exclusions::default(), limits).tick(&mut screen, running));
        assert_eq!(snapshot.nodes, 4);
    }

    /// Ticks and, like the capture loop, remembers what was stored.
    fn tick_and_store(capturer: &mut Capturer, screen: &mut FakeScreen) -> Outcome {
        let outcome = capturer.tick(screen, running);
        if let Outcome::Captured(snapshot) = &outcome {
            capturer.stored(snapshot, Instant::now());
        }
        outcome
    }

    #[test]
    fn changed_exclusions_drop_held_content() {
        let mut capturer = capturer();
        let start = Instant::now();
        let first = capturer.tick(&mut notes(), running);
        let Outcome::Captured(snapshot) = &first else {
            panic!("{first:?}")
        };
        capturer.stored(snapshot, start);
        // Changed content, read too soon to be stored: held back.
        let mut changed = FakeScreen::new(
            "com.apple.Notes",
            "Notes",
            window("Q3 proposal", vec![text("a", "Secret plans")]),
        );
        let outcome = capturer.tick(&mut changed, running);
        assert!(
            capturer
                .due(outcome, start + Duration::from_secs(5))
                .is_empty()
        );
        // The user excludes Notes; switching away must not store the held note.
        capturer.set_exclusions(Exclusions {
            apps: vec!["com.apple.Notes".to_string()],
            ..Exclusions::default()
        });
        assert!(
            capturer
                .due(
                    Outcome::Skipped(SkipReason::Never),
                    start + Duration::from_secs(6)
                )
                .is_empty()
        );
    }

    #[test]
    fn changed_exclusions_apply_from_the_next_tick() {
        let mut capturer = capturer();
        assert!(matches!(
            capturer.tick(&mut notes(), running),
            Outcome::Captured(_)
        ));
        capturer.set_exclusions(Exclusions {
            apps: vec!["com.apple.Notes".to_string()],
            ..Exclusions::default()
        });
        let mut screen = notes();
        assert_eq!(
            capturer.tick(&mut screen, running),
            Outcome::Skipped(SkipReason::ExcludedByUser)
        );
        assert_eq!(screen.reads(), vec!["app"]);
    }

    #[test]
    fn unchanged_content_is_skipped() {
        let mut capturer = capturer();
        assert!(matches!(
            tick_and_store(&mut capturer, &mut notes()),
            Outcome::Captured(_)
        ));
        assert_eq!(capturer.tick(&mut notes(), running), Outcome::Unchanged);

        let mut changed = FakeScreen::new(
            "com.apple.Notes",
            "Notes",
            window("Q3 proposal", vec![text("a", "Meeting moved")]),
        );
        assert!(matches!(
            tick_and_store(&mut capturer, &mut changed),
            Outcome::Captured(_)
        ));
        // Back to the first content: it changed since the last capture.
        assert!(matches!(
            tick_and_store(&mut capturer, &mut notes()),
            Outcome::Captured(_)
        ));
    }

    #[test]
    fn content_that_was_not_stored_is_captured_again() {
        // Dropped because a pause began, or the store failed: nothing was
        // stored, so the next tick must not call it unchanged.
        let mut capturer = capturer();
        assert!(matches!(
            capturer.tick(&mut notes(), running),
            Outcome::Captured(_)
        ));
        assert!(matches!(
            capturer.tick(&mut notes(), running),
            Outcome::Captured(_)
        ));
    }

    #[test]
    fn browser_titles_that_are_addresses_keep_only_the_host() {
        let mut screen = FakeScreen::new(
            "com.google.Chrome",
            "Google Chrome",
            window(
                "intranet.example.com/reset?token=abc123 - Google Chrome",
                vec![web_area(
                    "https://intranet.example.com/reset?token=abc123",
                    vec![text("a", "Reset")],
                )],
            ),
        );
        let snapshot = captured(capturer().tick(&mut screen, running));
        assert_eq!(snapshot.window_title, "intranet.example.com");

        assert_eq!(
            title_without_address("https://docs.example.com/d/1x2y/edit#h"),
            "docs.example.com"
        );
        assert_eq!(title_without_address("example.com/a"), "example.com");
        // Ordinary titles are left alone, also with slashes in them.
        for title in [
            "Q3 proposal",
            "Inbox (3)",
            "fnyman-ctrl/lax: Lax",
            "example.com",
        ] {
            assert_eq!(title_without_address(title), title);
        }
    }

    #[test]
    fn line_breaks_count_towards_the_text_limit() {
        let limits = Limits {
            max_chars: 10,
            ..Limits::default()
        };
        let mut screen = FakeScreen::new(
            "com.example.app",
            "Example",
            window(
                "x",
                vec![text("a", "abcd"), text("b", "efgh"), text("c", "ij")],
            ),
        );
        let snapshot =
            captured(Capturer::new(Exclusions::default(), limits).tick(&mut screen, running));
        assert_eq!(snapshot.text, "abcd\nefgh");
        assert!(snapshot.truncated);
    }

    #[test]
    fn windows_without_title_or_text_are_not_captured() {
        let mut screen = FakeScreen::new("com.apple.finder", "Finder", window("", vec![]));
        assert_eq!(capturer().tick(&mut screen, running), Outcome::Empty);
        // A title alone is still worth keeping.
        let mut screen = FakeScreen::new("com.apple.Safari", "Safari", window("Startsida", vec![]));
        let mut screen_with_field = FakeScreen::new(
            "com.apple.Safari",
            "Safari",
            window("Startsida", vec![safari_toolbar("smart sökfält")]),
        );
        assert_eq!(
            capturer().tick(&mut screen, running),
            Outcome::Skipped(SkipReason::Never)
        );
        assert!(matches!(
            capturer().tick(&mut screen_with_field, running),
            Outcome::Captured(_)
        ));
    }

    fn snapshot(title: &str, text: &str) -> Snapshot {
        Snapshot {
            captured_at: Utc::now(),
            app_name: "Claude".to_string(),
            bundle_id: "com.anthropic.claudefordesktop".to_string(),
            window_title: title.to_string(),
            site: None,
            text: text.to_string(),
            truncated: false,
            nodes: 1,
            skipped_frames: 0,
        }
    }

    /// Runs `outcome` through the throttle at `seconds`, storing everything
    /// it hands out, and returns the texts stored.
    fn feed(
        throttle: &mut Throttle,
        start: Instant,
        seconds: u64,
        outcome: Outcome,
    ) -> Vec<String> {
        let now = start + Duration::from_secs(seconds);
        let due = throttle.after(outcome, now);
        for snapshot in &due {
            throttle.stored(snapshot, now);
        }
        due.into_iter().map(|s| s.text).collect()
    }

    fn changed(title: &str, text: &str) -> Outcome {
        Outcome::Captured(snapshot(title, text))
    }

    #[test]
    fn a_window_that_keeps_changing_is_stored_at_most_every_gap() {
        let mut throttle = Throttle::new(Duration::from_secs(30));
        let t = Instant::now();
        assert_eq!(feed(&mut throttle, t, 0, changed("Chat", "a")), ["a"]);
        assert!(feed(&mut throttle, t, 5, changed("Chat", "ab")).is_empty());
        assert!(feed(&mut throttle, t, 10, changed("Chat", "abc")).is_empty());
        // Gap up: the latest content is stored, the ones in between never.
        assert_eq!(
            feed(&mut throttle, t, 30, changed("Chat", "abcd")),
            ["abcd"]
        );
    }

    #[test]
    fn held_content_is_stored_once_the_window_stops_changing() {
        let mut throttle = Throttle::new(Duration::from_secs(30));
        let t = Instant::now();
        feed(&mut throttle, t, 0, changed("Chat", "a"));
        assert!(feed(&mut throttle, t, 5, changed("Chat", "final")).is_empty());
        assert!(feed(&mut throttle, t, 10, Outcome::Unchanged).is_empty());
        assert_eq!(feed(&mut throttle, t, 30, Outcome::Unchanged), ["final"]);
        assert!(feed(&mut throttle, t, 35, Outcome::Unchanged).is_empty());
    }

    #[test]
    fn switching_window_stores_what_was_held_first() {
        let mut throttle = Throttle::new(Duration::from_secs(30));
        let t = Instant::now();
        feed(&mut throttle, t, 0, changed("Chat", "a"));
        feed(&mut throttle, t, 5, changed("Chat", "last words"));
        assert_eq!(
            feed(&mut throttle, t, 10, changed("Notes", "note")),
            ["last words", "note"]
        );
        // Also when the next window is skipped or empty.
        feed(&mut throttle, t, 15, changed("Notes", "note 2"));
        assert_eq!(
            feed(&mut throttle, t, 20, Outcome::Skipped(SkipReason::Never)),
            ["note 2"]
        );
    }

    #[test]
    fn held_content_keeps_the_time_it_was_read() {
        let mut throttle = Throttle::new(Duration::from_secs(30));
        let t = Instant::now();
        feed(&mut throttle, t, 0, changed("Chat", "a"));
        let read = snapshot("Chat", "read at 10:00:05");
        let read_at = read.captured_at;
        assert!(
            throttle
                .after(Outcome::Captured(read), t + Duration::from_secs(5))
                .is_empty()
        );
        let due = throttle.after(
            Outcome::Captured(snapshot("Notes", "n")),
            t + Duration::from_secs(29),
        );
        assert_eq!(due[0].captured_at, read_at);
    }

    #[test]
    fn a_pause_drops_what_was_held() {
        let mut throttle = Throttle::new(Duration::from_secs(30));
        let t = Instant::now();
        feed(&mut throttle, t, 0, changed("Chat", "a"));
        feed(
            &mut throttle,
            t,
            5,
            changed("Chat", "read before the pause"),
        );
        assert!(feed(&mut throttle, t, 10, Outcome::Paused).is_empty());
        assert!(feed(&mut throttle, t, 40, Outcome::Unchanged).is_empty());
    }

    #[test]
    fn repeated_lines_are_kept_once() {
        let mut screen = FakeScreen::new(
            "com.example.app",
            "Example",
            window(
                "x",
                vec![text("a", "Same"), text("b", " Same "), text("c", "")],
            ),
        );
        assert_eq!(captured(capturer().tick(&mut screen, running)).text, "Same");
    }

    #[test]
    fn no_window_reads_nothing_more() {
        let mut screen = notes();
        screen.window = None;
        assert_eq!(capturer().tick(&mut screen, running), Outcome::NoWindow);
        assert_eq!(screen.reads(), vec!["app", "window"]);
    }

    #[test]
    fn browser_names_and_profiles_are_dropped_from_titles() {
        let clean = |t, app| clean_title(t, app, true);
        assert_eq!(
            clean("Q3 - Google Docs - Google Chrome", "Google Chrome"),
            "Q3 - Google Docs"
        );
        assert_eq!(clean("Q3 - Google Chrome - Anna", "Google Chrome"), "Q3");
        assert_eq!(clean("Example — Mozilla Firefox", "Firefox"), "Example");
        assert_eq!(
            clean("Example - Microsoft Edge", "Microsoft Edge"),
            "Example"
        );
        assert_eq!(clean("Example Domain", "Safari"), "Example Domain");
        // Other apps keep their titles as they are.
        assert_eq!(
            clean_title("Notes - Notes", "Notes", false),
            "Notes - Notes"
        );
    }

    #[test]
    fn reading_slows_down_when_the_user_is_away() {
        let s = Duration::from_secs;
        let five = s(5);
        assert_eq!(pace(five, None, false), five);
        assert_eq!(pace(five, Some(s(30)), false), five);
        assert_eq!(pace(five, Some(s(60)), false), s(15));
        assert_eq!(pace(five, Some(s(600)), false), s(60));
        // A longer chosen interval is never shortened.
        assert_eq!(pace(s(120), Some(s(60)), false), s(120));
    }

    #[test]
    fn reading_slows_down_on_battery() {
        let s = Duration::from_secs;
        assert_eq!(pace(s(5), None, true), s(10));
        assert_eq!(pace(s(5), Some(s(600)), true), s(120));
        // Capped at five minutes, unless the user chose longer.
        assert_eq!(pace(s(200), None, true), s(300));
        assert_eq!(pace(s(300), None, true), s(300));
    }

    #[test]
    fn heartbeat_tells_whether_the_loop_runs() {
        let heartbeat = Heartbeat::default();
        let at = Duration::from_secs;
        assert!(!heartbeat.alive_at(at(0)), "never beat");
        heartbeat.beat_at(at(100), at(5));
        assert!(heartbeat.alive_at(at(100 + 5 + 15)));
        assert!(!heartbeat.alive_at(at(100 + 5 + 16)));
    }

    #[test]
    fn heartbeat_allows_the_longer_waits_while_away() {
        let heartbeat = Heartbeat::default();
        let at = Duration::from_secs;
        // Away and on battery: the loop says it waits two minutes.
        heartbeat.beat_at(at(100), at(120));
        assert!(heartbeat.alive_at(at(100 + 120)));
        assert!(!heartbeat.alive_at(at(100 + 120 + 16)));
    }

    #[test]
    fn truncation_keeps_whole_characters() {
        assert_eq!(truncate_chars("åäö", 2), "åä");
        assert_eq!(truncate_chars("abc", 10), "abc");
        assert_eq!(truncate_chars("", 1), "");
    }

    #[test]
    fn stored_captures_are_redacted_and_keep_only_the_host() {
        let store = crate::store::Store::open_in_memory().unwrap();
        let mut screen = FakeScreen::new(
            "com.google.Chrome",
            "Google Chrome",
            window(
                "Settings - Google Chrome",
                vec![web_area(
                    "https://console.example.com/keys?id=7",
                    vec![text("a", "Personnummer 19811218-9876")],
                )],
            ),
        );
        let snapshot = captured(capturer().tick(&mut screen, running));
        store
            .insert(&crate::store::NewCapture {
                captured_at: snapshot.captured_at,
                app_name: &snapshot.app_name,
                bundle_id: &snapshot.bundle_id,
                window_title: &snapshot.window_title,
                site: snapshot.site.as_deref(),
                text: &snapshot.text,
            })
            .unwrap();
        let rows = store.search("Personnummer", 10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].site.as_deref(), Some("console.example.com"));
        assert_eq!(rows[0].window_title, "Settings");
        assert!(!rows[0].text.contains("19811218-9876"), "{}", rows[0].text);
    }
}
