//! Calling Lax to the text field the user is in by double-tapping ⌥ Option
//! (ADR 0009). Nothing appears in text fields until then.
//!
//! To tell a tap of ⌥ from ⌥2 (@), Lax has to notice that some key went down
//! while ⌥ was held. macOS hands every key event over with its key code; the
//! handler turns it into an [`Input`], which has no room for a key, before
//! anything else runs. Nothing about the events is stored or logged, and
//! nothing is looked at while an excluded app is in front. Password fields
//! turn on Secure Event Input, which keeps their keys from every listener.
//!
//! Listening needs the Accessibility permission Lax already has (an
//! `NSEvent` global monitor), not Input Monitoring. It runs only while the
//! user has turned the writing assistant on.

use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Manager};

#[cfg(target_os = "macos")]
use crate::ax::FieldText;
use crate::exclusions::{Current, Decision, Exclusions};
use crate::logging::{self, Field};
use crate::pause::Pause;
use crate::store::Store;

/// All Lax keeps of a key or mouse event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input {
    /// A key or mouse button went down. Which one is never known here.
    Pressed,
    /// The modifier keys held changed; this is what is held now.
    Modifiers(Held),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Held {
    /// No modifier key (Caps Lock doesn't count).
    #[default]
    Nothing,
    /// The key that calls Lax and nothing else.
    KeyOnly,
    /// Anything else, such as ⌘ or ⇧, alone or with that key.
    Other,
}

/// The modifier key the user double-taps to call Lax: ⌥ Option unless
/// they pick another, for instance because another app (such as a
/// dictation app) already uses it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CallKey {
    #[default]
    Option,
    /// Only the right ⌥ Option, so the left one stays free.
    RightOption,
    Control,
    Command,
    Shift,
}

impl CallKey {
    pub fn as_str(self) -> &'static str {
        match self {
            CallKey::Option => "option",
            CallKey::RightOption => "rightOption",
            CallKey::Control => "control",
            CallKey::Command => "command",
            CallKey::Shift => "shift",
        }
    }

    pub fn parse(text: &str) -> Option<CallKey> {
        [
            CallKey::Option,
            CallKey::RightOption,
            CallKey::Control,
            CallKey::Command,
            CallKey::Shift,
        ]
        .into_iter()
        .find(|key| key.as_str() == text)
    }

    fn flag(self) -> usize {
        match self {
            CallKey::Option | CallKey::RightOption => OPTION,
            CallKey::Control => CONTROL,
            CallKey::Command => COMMAND,
            CallKey::Shift => SHIFT,
        }
    }

    /// Whether the right one of the pair is held, where that matters.
    fn side(self, flags: usize) -> bool {
        match self {
            CallKey::RightOption => flags & RIGHT_OPTION != 0 && flags & LEFT_OPTION == 0,
            _ => true,
        }
    }
}

/// macOS modifier flags (`NSEventModifierFlags`), and the device-dependent
/// bits that tell the left ⌥ from the right one (`NX_DEVICELALTKEYMASK`,
/// `NX_DEVICERALTKEYMASK`). They say which modifier keys are held, never
/// which other key.
const CAPS_LOCK: usize = 1 << 16;
const SHIFT: usize = 1 << 17;
const CONTROL: usize = 1 << 18;
const OPTION: usize = 1 << 19;
const COMMAND: usize = 1 << 20;
const NUMERIC_PAD: usize = 1 << 21;
const DEVICE_INDEPENDENT: usize = 0xffff_0000;
const LEFT_OPTION: usize = 0x20;
const RIGHT_OPTION: usize = 0x40;

/// Which modifiers `flags` (an event's modifier flags) say are held, for
/// the key that calls Lax.
pub fn held(flags: usize, key: CallKey) -> Held {
    // Caps Lock is a state, not a key held; the numeric pad flag comes with
    // arrow keys. Neither keeps the key from being alone.
    match flags & DEVICE_INDEPENDENT & !(CAPS_LOCK | NUMERIC_PAD) {
        0 => Held::Nothing,
        held if held == key.flag() && key.side(flags) => Held::KeyOnly,
        _ => Held::Other,
    }
}

/// A tap is ⌥ pressed and released within this time, with nothing between.
const TAP: Duration = Duration::from_millis(500);
/// The second tap must start within this time of the first ending.
const GAP: Duration = Duration::from_millis(400);

/// Tells a double tap of ⌥ Option, on its own, from everything else: ⌥2,
/// ⌥-click, holding ⌥, a single tap, ⌥ with ⇧.
#[derive(Default)]
pub struct DoubleTap {
    held: Held,
    /// When ⌥ went down, while it is held alone.
    down: Option<Instant>,
    /// When the last tap ended, while it may be the first of two.
    tapped: Option<Instant>,
}

impl DoubleTap {
    /// Takes the next input; `true` when it completes a double tap.
    pub fn input(&mut self, input: Input, now: Instant) -> bool {
        let held = match input {
            Input::Pressed => {
                self.reset();
                return false;
            }
            Input::Modifiers(held) => held,
        };
        let before = std::mem::replace(&mut self.held, held);
        match (before, held) {
            (Held::Nothing, Held::KeyOnly) => {
                if self
                    .tapped
                    .is_some_and(|end| now.saturating_duration_since(end) > GAP)
                {
                    self.tapped = None;
                }
                self.down = Some(now);
                false
            }
            (Held::KeyOnly, Held::Nothing) => {
                let Some(down) = self.down.take() else {
                    return false;
                };
                if now.saturating_duration_since(down) > TAP {
                    self.tapped = None;
                    return false;
                }
                if self.tapped.take().is_some() {
                    return true;
                }
                self.tapped = Some(now);
                false
            }
            _ => {
                self.reset();
                false
            }
        }
    }

    /// Forgets any tap in progress, such as when an excluded app comes to the
    /// front.
    pub fn reset(&mut self) {
        self.down = None;
        self.tapped = None;
    }
}

/// Whether Lax may come when called: the user turned it on, capture isn't
/// paused, and the app in front isn't one Lax may not read.
fn may_come(enabled: bool, paused: bool, app: Decision) -> bool {
    enabled && !paused && app == Decision::Capture
}

/// What Lax was called to: the app to give focus back to, the user's
/// instruction from the field, and the window's text as context. Held in
/// memory only while the panel is open; never stored or logged.
#[derive(Default)]
pub struct Summoned(Mutex<Option<Call>>);

#[derive(Clone, Debug, Default)]
pub struct Call {
    /// Different for every time Lax is called, so the panel can tell a new
    /// call from coming back to the same one.
    pub id: u64,
    pub pid: i32,
    pub app_name: String,
    /// The text selected in the field, or what is in it. Secrets redacted.
    pub instruction: String,
    /// The window's title and text, read under capture's rules. Secrets
    /// redacted. `None` until read, or if there was nothing to read.
    pub context: Option<String>,
    /// Whether the window is still being read.
    pub reading: bool,
    /// What a draft replaces in the field.
    pub replace: Replace,
    /// Whether the field can be written through Accessibility. Not when the
    /// app named a field hidden behind another window: then the draft is
    /// pasted where the keyboard focus really is.
    pub accessible: bool,
}

/// An accessibility role as a number for the log, which takes no text:
/// 1 text area, 2 text field, 3 web area, 4 group, 5 scroll area, 6 static
/// text, 7 layout area, 8 combo box, 9 text view (Pages), 0 anything else
/// or none.
pub fn role_code(role: Option<&str>) -> i64 {
    match role {
        Some("AXTextArea") => 1,
        Some("AXTextField") => 2,
        Some("AXWebArea") => 3,
        Some("AXGroup") => 4,
        Some("AXScrollArea") => 5,
        Some("AXStaticText") => 6,
        Some("AXLayoutArea") => 7,
        Some("AXComboBox") => 8,
        Some("AXTextView") => 9,
        _ => 0,
    }
}

/// The roles of fields people type in, which Lax may come to even when
/// they don't say where their text cursor is (it then uses their first line).
pub const TYPED_IN: &[&str] = &["AXTextArea", "AXTextField", "AXComboBox", "AXTextView"];

/// Whether `role` is one of [`TYPED_IN`].
pub fn typed_in(role: Option<&str>) -> bool {
    role.is_some_and(|role| TYPED_IN.contains(&role))
}

/// What a draft replaces, as a number for the log: 0 nothing, 1 the
/// selection, 2 a paragraph, 3 the whole field.
pub fn replace_code(replace: Replace) -> i64 {
    match replace {
        Replace::Nothing => 0,
        Replace::Selection => 1,
        Replace::Paragraph { whole: false, .. } => 2,
        Replace::Paragraph { whole: true, .. } => 3,
    }
}

/// The id of the next call.
pub(crate) static NEXT_CALL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// What a draft replaces in the field Lax was called to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Replace {
    /// Nothing: it goes in at the cursor (the instruction was typed in the
    /// panel).
    #[default]
    Nothing,
    /// The selected text, which was the instruction.
    Selection,
    /// The paragraph the user wrote the instruction in: `len` UTF-16 units
    /// from `start`. `whole` if that is all of the field's text.
    Paragraph {
        start: usize,
        len: usize,
        whole: bool,
    },
}

/// The paragraph of `text` around the cursor at `caret` (in UTF-16 units):
/// its text, and its start and length in UTF-16 units. In a document (Notes,
/// Pages, a mail) that is what the user just wrote, not the whole document;
/// in a one-line field it is all of it.
pub fn paragraph_at(text: &str, caret: usize) -> (String, usize, usize) {
    let is_break = |c: char| matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}');
    let (mut start, mut end) = (0, None);
    let mut offset = 0;
    for c in text.chars() {
        if is_break(c) {
            if offset < caret {
                start = offset + c.len_utf16();
            } else {
                end = Some(offset);
                break;
            }
        }
        offset += c.len_utf16();
    }
    let end = end.unwrap_or(offset);
    let units: Vec<u16> = text.encode_utf16().collect();
    let paragraph = String::from_utf16_lossy(&units[start.min(end)..end]);
    (paragraph, start.min(end), end.saturating_sub(start))
}

/// The instruction in a field and what a draft replaces: the selection if
/// there is one, else the paragraph at the cursor, else all of it.
#[cfg(target_os = "macos")]
pub fn instruction_in(text: &FieldText) -> (String, Replace) {
    let (instruction, replace) = match text {
        FieldText::Selected(selected) => (instruction_from(selected), Replace::Selection),
        FieldText::All { value, caret } => {
            let total = value.encode_utf16().count();
            let (paragraph, start, len) = match caret {
                Some(caret) if *caret <= total => paragraph_at(value, *caret),
                _ => (value.clone(), 0, total),
            };
            let whole = start == 0 && len == total;
            (
                instruction_from(&paragraph),
                Replace::Paragraph { start, len, whole },
            )
        }
    };
    if instruction.is_empty() {
        return (instruction, Replace::Nothing);
    }
    (instruction, replace)
}

impl Summoned {
    pub fn call(&self) -> Option<Call> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Forgets the call, such as when the panel goes away.
    pub fn forget(&self) -> Option<Call> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).take()
    }

    pub(crate) fn set(&self, call: Option<Call>) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = call;
    }

    /// Adds the window's text, if any, to the call for `pid`, if it is
    /// still open, and marks the reading done.
    fn set_context(&self, pid: i32, context: Option<String>) {
        let mut call = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(call) = call.as_mut().filter(|call| call.pid == pid) {
            call.context = context;
            call.reading = false;
        }
    }
}

/// The instruction in a field's text (selected, or all of it): the text,
/// secrets redacted, unless it is longer than an instruction usually is, such
/// as a whole document with nothing selected. Then the user types one.
pub fn instruction_from(text: &str) -> String {
    let text = text.trim();
    if text.chars().count() > MAX_FIELD_INSTRUCTION_CHARS {
        return String::new();
    }
    crate::redact::redact(text)
}

/// Longer field text than this isn't taken as the instruction.
const MAX_FIELD_INSTRUCTION_CHARS: usize = 400;

/// Characters of the field's text and of the window's taken along.
pub const MAX_INSTRUCTION_CHARS: usize = 2000;
pub const MAX_CONTEXT_CHARS: usize = 6000;

/// The panel's window label. Its capability is `capabilities/panel.json`.
pub const PANEL: &str = "panel";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WritingStatus {
    enabled: bool,
    key: CallKey,
}

/// The key the user picked to call Lax.
fn call_key(app: &AppHandle) -> CallKey {
    app.try_state::<Mutex<Store>>()
        .and_then(|store| {
            let store = store.lock().unwrap_or_else(PoisonError::into_inner);
            store.writing_key().ok().flatten()
        })
        .and_then(|key| CallKey::parse(&key))
        .unwrap_or_default()
}

fn enabled(app: &AppHandle) -> bool {
    app.try_state::<Mutex<Store>>().is_some_and(|store| {
        let store = store.lock().unwrap_or_else(PoisonError::into_inner);
        store.writing_enabled().unwrap_or(false)
    })
}

#[tauri::command(async)]
pub fn writing_status(app: AppHandle) -> Result<WritingStatus, String> {
    let store = app
        .try_state::<Mutex<Store>>()
        .ok_or("The database isn't available.")?;
    let store = store.lock().unwrap_or_else(PoisonError::into_inner);
    let enabled = store
        .writing_enabled()
        .map_err(|_| "Lax couldn't read the setting.")?;
    let key = store
        .writing_key()
        .map_err(|_| "Lax couldn't read the setting.")?
        .and_then(|key| CallKey::parse(&key))
        .unwrap_or_default();
    Ok(WritingStatus { enabled, key })
}

/// Picks the key that calls Lax, and listens for it from now on.
#[tauri::command(async)]
pub fn set_writing_key(app: AppHandle, key: CallKey) -> Result<WritingStatus, String> {
    {
        let store = app
            .try_state::<Mutex<Store>>()
            .ok_or("The database isn't available.")?;
        let store = store.lock().unwrap_or_else(PoisonError::into_inner);
        store
            .set_writing_key(key.as_str())
            .map_err(|_| "Lax couldn't save the setting.")?;
    }
    start(&app);
    writing_status(app)
}

#[tauri::command(async)]
pub fn set_writing_enabled(app: AppHandle, enabled: bool) -> Result<WritingStatus, String> {
    {
        let store = app
            .try_state::<Mutex<Store>>()
            .ok_or("The database isn't available.")?;
        let store = store.lock().unwrap_or_else(PoisonError::into_inner);
        store
            .set_writing_enabled(enabled)
            .map_err(|_| "Lax couldn't save the setting.")?;
    }
    if enabled {
        start(&app);
    } else {
        stop(&app);
    }
    writing_status(app)
}

/// Closes the panel and gives focus back to the app Lax was called to.
#[tauri::command]
pub fn close_panel(app: AppHandle) {
    if let Some(panel) = app.get_webview_window(PANEL) {
        let _ = panel.hide();
    }
    let pid = app.state::<Summoned>().forget().map(|call| call.pid);
    #[cfg(target_os = "macos")]
    if let Some(pid) = pid {
        let _ = app.run_on_main_thread(move || macos::activate(pid));
    }
    #[cfg(not(target_os = "macos"))]
    let _ = pid;
}

/// Starts listening if the user turned the assistant on and Lax has the
/// Accessibility permission. Called at start, when the permission comes and
/// when the user turns it on.
///
/// Without the permission macOS still delivers modifier changes but not key
/// presses, so ⌥2 typed twice would look like a double tap.
pub fn start(app: &AppHandle) {
    if enabled(app) && crate::accessibility::is_granted() {
        listen(app);
    }
}

/// Starts (or restarts) listening.
fn listen(app: &AppHandle) {
    #[cfg(target_os = "macos")]
    {
        let handle = app.clone();
        let _ = app.run_on_main_thread(move || macos::install(handle));
        logging::info("write.listening", &[]);
    }
    #[cfg(not(target_os = "macos"))]
    let _ = app;
}

/// Stops listening.
pub fn stop(app: &AppHandle) {
    #[cfg(target_os = "macos")]
    {
        let _ = app.run_on_main_thread(macos::remove);
        logging::info("write.stopped", &[]);
    }
    #[cfg(not(target_os = "macos"))]
    let _ = app;
}

/// A rectangle on screen, in points, from the top left of the main display
/// (or of a window, where said).
#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Deserialize)]
#[repr(C)] // Read from an AXValue as a CGRect.
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    /// Finite, not negative in size, and not absurdly large: what a card in
    /// the panel can be.
    pub fn is_sane(&self) -> bool {
        [self.x, self.y, self.width, self.height]
            .iter()
            .all(|v| v.is_finite() && v.abs() < 10_000.0)
            && self.width >= 0.0
            && self.height >= 0.0
    }
}

/// The panel window: room for the salmon to swim in from the left, and the
/// card below the line, no wider than the text field (within limits).
/// Matches `.panel` in `src/styles.css`.
const PANEL_HEIGHT: f64 = 100.0;
/// Where in the window the salmon's nose ends up, which is put on the text
/// cursor: this far from the left edge, at this height (the middle of the
/// salmon, so it swims along the line of text).
const NOSE_X: f64 = 200.0;
const NOSE_Y: f64 = 11.0;
/// Where the card starts, and the room right of it for its shadow.
const CARD_LEFT: f64 = 160.0;
const CARD_MARGIN: f64 = 12.0;
/// The card's width: the text field's, within these limits.
const CARD_MIN: f64 = 180.0;
const CARD_MAX: f64 = 320.0;
/// For a field of unknown width.
const CARD_DEFAULT: f64 = 260.0;

/// Where the panel goes and how wide it is, for the text cursor at `caret`
/// in a field `field_width` wide: the salmon's nose on the cursor, in the
/// middle of the line, and the card no wider than the field. Kept on
/// `screen`, even if the salmon then stops a little off the cursor.
fn place(caret: Rect, field_width: Option<f64>, screen: Rect) -> (f64, f64, f64) {
    let card = field_width.map_or(CARD_DEFAULT, |w| w.clamp(CARD_MIN, CARD_MAX));
    let width = CARD_LEFT + card + CARD_MARGIN;
    let x = caret.x - NOSE_X;
    let y = caret.y + caret.height / 2.0 - NOSE_Y;
    let right = (screen.x + screen.width - width).max(screen.x);
    let bottom = (screen.y + screen.height - PANEL_HEIGHT).max(screen.y);
    (x.clamp(screen.x, right), y.clamp(screen.y, bottom), width)
}

/// Comes to the text field, if Lax may. Called on the main thread when the
/// user double-taps ⌥ Option; the window is looked at on a thread of its
/// own, since another app can take a while to answer.
fn come(app: &AppHandle, app_decision: Decision) {
    let paused = app.state::<Pause>().is_paused();
    if !may_come(enabled(app), paused, app_decision) {
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        #[cfg(target_os = "macos")]
        if let Some(found) = macos::locate(&app) {
            let (pid, front) = (found.call.pid, found.front.clone());
            show(&app, found.call, found.caret, found.field);
            // The window is read after the salmon is on its way: under the
            // same rules as capture, for the app Lax was called to (Lax's
            // own panel is in front by now). If it turns out to be a site
            // the user excluded, Lax goes away again before anything is used.
            match macos::read_window(&app, front) {
                Ok(context) => app.state::<Summoned>().set_context(pid, context),
                Err(()) => {
                    if let Some(panel) = app.get_webview_window(PANEL) {
                        let _ = panel.hide();
                    }
                    app.state::<Summoned>().forget();
                }
            }
        }
        #[cfg(not(target_os = "macos"))]
        let _ = app;
    });
}

/// Shows the panel at `caret` in `field`, or by the mouse pointer if the app
/// didn't say where its text cursor is.
fn show(app: &AppHandle, call: Call, caret: Option<Rect>, field: Option<Rect>) {
    logging::info(
        "write.summoned",
        &[Field::Flag("at_cursor", caret.is_some())],
    );
    app.state::<Summoned>().set(Some(call));
    let panel = match app.get_webview_window(PANEL) {
        Some(panel) => panel,
        None => {
            let built = tauri::WebviewWindowBuilder::new(
                app,
                PANEL,
                tauri::WebviewUrl::App("index.html".into()),
            )
            .title("Lax")
            .inner_size(CARD_LEFT + CARD_DEFAULT + CARD_MARGIN, PANEL_HEIGHT)
            .decorations(false)
            .transparent(true)
            .shadow(false)
            .resizable(false)
            .always_on_top(true)
            .skip_taskbar(true)
            .visible(false)
            .build();
            match built {
                Ok(panel) => panel,
                Err(_) => return,
            }
        }
    };
    let Some(caret) = caret.or_else(|| pointer(app)) else {
        return;
    };
    let screen = screen_of(app, caret).unwrap_or(Rect {
        x: caret.x - 2.0 * NOSE_X,
        y: caret.y - PANEL_HEIGHT,
        width: 4.0 * NOSE_X + CARD_MAX,
        height: 3.0 * PANEL_HEIGHT,
    });
    // The glass from last time stays hidden until the card opens again.
    crate::glass::set(&panel, None);
    let (x, y, width) = place(caret, field.map(|field| field.width), screen);
    let _ = panel.set_size(tauri::LogicalSize::new(width, PANEL_HEIGHT));
    let _ = panel.set_position(tauri::LogicalPosition::new(x, y));
    let _ = panel.show();
    let _ = panel.set_focus();
}

/// The mouse pointer as a one-line "cursor", in points.
fn pointer(app: &AppHandle) -> Option<Rect> {
    let cursor = app.cursor_position().ok()?;
    let scale = app
        .monitor_from_point(cursor.x, cursor.y)
        .ok()
        .flatten()
        .map_or(1.0, |monitor| monitor.scale_factor());
    Some(Rect {
        x: cursor.x / scale,
        y: cursor.y / scale,
        width: 0.0,
        height: 16.0,
    })
}

/// The display `caret` is on, in points.
fn screen_of(app: &AppHandle, caret: Rect) -> Option<Rect> {
    app.available_monitors()
        .ok()?
        .into_iter()
        .find_map(|monitor| {
            let scale = monitor.scale_factor();
            let position = monitor.position().to_logical::<f64>(scale);
            let size = monitor.size().to_logical::<f64>(scale);
            let screen = Rect {
                x: position.x,
                y: position.y,
                width: size.width,
                height: size.height,
            };
            let inside = caret.x >= screen.x
                && caret.x < screen.x + screen.width
                && caret.y >= screen.y
                && caret.y < screen.y + screen.height;
            inside.then_some(screen)
        })
}

/// What the event handler keeps between events, on the main thread.
#[derive(Default)]
struct Listener {
    tap: DoubleTap,
    /// The user's exclusions and their version in [`Current`].
    exclusions: (u64, Exclusions),
}

impl Listener {
    /// Takes one input while `bundle_id` is in front; `Some` with the app's
    /// decision when Lax is called.
    fn input(&mut self, current: &Current, bundle_id: &str, input: Input) -> Option<Decision> {
        if let Some(newer) = current.newer_than(self.exclusions.0) {
            self.exclusions = newer;
        }
        let decision = self.exclusions.1.check_app(bundle_id);
        if decision != Decision::Capture {
            // Nothing is looked at in an app Lax may not read.
            self.tap.reset();
            return None;
        }
        self.tap.input(input, Instant::now()).then_some(decision)
    }
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)] // Two AppKit calls; see SAFETY below.
pub(crate) mod macos {
    use std::cell::RefCell;
    use std::ptr::NonNull;

    use block2::RcBlock;
    use objc2::rc::{Retained, autoreleasepool};
    use objc2::runtime::AnyObject;
    use objc2_app_kit::{
        NSApplicationActivationOptions, NSEvent, NSEventMask, NSEventType, NSRunningApplication,
        NSWorkspace,
    };
    use tauri::{AppHandle, Manager};

    use super::{
        Call, CallKey, Input, Listener, MAX_CONTEXT_CHARS, MAX_INSTRUCTION_CHARS, Rect, come, held,
        instruction_in,
    };
    use crate::ax::{AxScreen, FieldText, window_owner_at};
    use crate::capture::{App, Capturer, Limits, Node, Outcome, Screen, admit, truncate_chars};
    use crate::exclusions::{Current, is_secure_text_field};
    use crate::logging::{self, Field};
    use crate::pause::Pause;
    use crate::redact::redact;

    thread_local! {
        /// The installed monitor. AppKit objects stay on the main thread.
        static MONITOR: RefCell<Option<Retained<AnyObject>>> = const { RefCell::new(None) };
    }

    /// The only place that sees an event. It keeps whether it was a press
    /// or a change of modifiers, and which modifiers; never the key.
    fn reduce(event: &NSEvent, key: CallKey) -> Input {
        if event.r#type() == NSEventType::FlagsChanged {
            Input::Modifiers(held(event.modifierFlags().0, key))
        } else {
            Input::Pressed
        }
    }

    pub fn install(app: AppHandle) {
        remove();
        let key = super::call_key(&app);
        let listener = RefCell::new(Listener::default());
        let block = RcBlock::new(move |event: NonNull<NSEvent>| {
            // SAFETY: AppKit passes a valid event for the duration of the call.
            let input = reduce(unsafe { event.as_ref() }, key);
            let Some(bundle_id) = frontmost() else {
                listener.borrow_mut().tap.reset();
                return;
            };
            let called = listener
                .borrow_mut()
                .input(&app.state::<Current>(), &bundle_id, input);
            if let Some(decision) = called {
                come(&app, decision);
            }
        });
        let mask = NSEventMask::KeyDown
            | NSEventMask::FlagsChanged
            | NSEventMask::LeftMouseDown
            | NSEventMask::RightMouseDown
            | NSEventMask::OtherMouseDown;
        let monitor = NSEvent::addGlobalMonitorForEventsMatchingMask_handler(mask, &block);
        MONITOR.with(|slot| *slot.borrow_mut() = monitor);
    }

    pub fn remove() {
        MONITOR.with(|slot| {
            if let Some(monitor) = slot.borrow_mut().take() {
                // SAFETY: `monitor` was returned by `addGlobalMonitor…`.
                unsafe { NSEvent::removeMonitor(&monitor) };
            }
        });
    }

    /// The bundle identifier of the app in front.
    fn frontmost() -> Option<String> {
        autoreleasepool(|_| {
            let app = NSWorkspace::sharedWorkspace().frontmostApplication()?;
            Some(app.bundleIdentifier()?.to_string())
        })
    }

    /// What [`locate`] found.
    pub struct Found {
        pub front: App,
        pub call: Call,
        pub caret: Option<Rect>,
        pub field: Option<Rect>,
    }

    /// How Lax found the field, as a number for the log: 0 the app named it
    /// as focused, 1 inside the focused scroll area, 2 in the window, 3 not
    /// at all (Lax goes by the mouse pointer).
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Way {
        Focused = 0,
        InArea = 1,
        InWindow = 2,
        Pointer = 3,
    }

    /// How many levels down Lax looks for the field: inside a focused scroll
    /// area, and in the whole window (web apps nest deeply).
    const FIELD_DEPTH_IN_AREA: usize = 6;
    const FIELD_DEPTH_IN_WINDOW: usize = 30;

    /// Where the text cursor is in the app in front, and the instruction in
    /// the field, after the same checks capture makes before reading a
    /// window: the app, the window title and whether a browser window is
    /// private. `None` if Lax may not come: one of those says no, capture is
    /// paused, or the focus is in a password field.
    pub fn locate(app: &AppHandle) -> Option<Found> {
        // Before the user's exclusions are loaded, Lax comes nowhere.
        let (_, exclusions) = app.state::<Current>().newer_than(0)?;
        let pause = app.state::<Pause>();
        let mut screen = AxScreen::default();
        let (front, _window, _title) = admit(&mut screen, &exclusions, &Limits::default(), &|| {
            pause.is_paused()
        })
        .ok()?;
        let mut found = Found {
            call: Call {
                id: super::NEXT_CALL.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
                pid: front.pid,
                app_name: front.name.clone(),
                reading: true,
                ..Call::default()
            },
            front,
            caret: None,
            field: None,
        };
        let Some(focused) = screen.focused_element(&found.front) else {
            return Some(found);
        };
        let (role, subrole) = focused.role_and_subrole();
        if is_secure_text_field(role.as_deref().unwrap_or_default(), subrole.as_deref()) {
            return None;
        }
        // Some apps (Pages, Claude) name a whole scroll area as focused, not
        // the field the user types in. Then the field inside it, if one says
        // where its cursor is; otherwise the mouse pointer, not the area's
        // top left corner.
        let (element, role, way) = if super::typed_in(role.as_deref()) || focused.has_caret_bounds()
        {
            (focused, role, Way::Focused)
        } else if let Some((field, way)) = focused
            .field_within(super::TYPED_IN, false, FIELD_DEPTH_IN_AREA)
            .map(|field| (field, Way::InArea))
            .or_else(|| {
                // Not inside it (Claude's composer): the field in the window
                // that says it has the keyboard focus.
                let window = screen.focused_window(&found.front)?;
                window
                    .field_within(super::TYPED_IN, true, FIELD_DEPTH_IN_WINDOW)
                    .map(|field| (field, Way::InWindow))
            })
        {
            // A focused password field comes back so that Lax stays away.
            let (role, subrole) = field.role_and_subrole();
            if is_secure_text_field(role.as_deref().unwrap_or_default(), subrole.as_deref()) {
                return None;
            }
            (field, role, way)
        } else {
            log_field(
                role.as_deref(),
                false,
                false,
                Way::Pointer,
                None,
                &found.call,
            );
            return Some(found);
        };
        let inner = way != Way::Focused;
        let caret = element.caret();
        // Some apps (Electron apps with several web views, such as Claude)
        // name a field as focused that is hidden: behind another app's
        // window, or behind another view of the same window. If something
        // else shows where the cursor is said to be, go by the mouse pointer
        // instead, and don't take that field's text.
        let hidden = caret.is_some_and(|caret| {
            let (x, y) = (caret.x + 1.0, caret.y + caret.height / 2.0);
            window_owner_at(x, y).is_some_and(|owner| owner != found.front.pid)
                || screen.shows_at(&found.front, &element, x, y) == Some(false)
        });
        if hidden {
            log_field(
                role.as_deref(),
                element.has_caret_bounds(),
                true,
                way,
                None,
                &found.call,
            );
            return Some(found);
        }
        // Inside a scroll area the draft is pasted at the keyboard focus, so
        // it can replace a selection but not select a paragraph itself.
        let text = element
            .field_text(MAX_INSTRUCTION_CHARS)
            .filter(|text| !inner || matches!(text, FieldText::Selected(_)));
        if let Some(text) = &text {
            (found.call.instruction, found.call.replace) = instruction_in(text);
        }
        log_field(
            role.as_deref(),
            element.has_caret_bounds(),
            false,
            way,
            text.as_ref(),
            &found.call,
        );
        found.call.accessible = !inner;
        found.caret = caret;
        found.field = element.frame();
        Some(found)
    }

    /// What kind of field Lax was called to and what it offered, as codes
    /// and counts only, never text: so a field that behaves oddly (Pages)
    /// can be understood from the log.
    fn log_field(
        role: Option<&str>,
        caret_bounds: bool,
        hidden: bool,
        way: Way,
        text: Option<&FieldText>,
        call: &Call,
    ) {
        let (selected, value_chars, caret_at) = match text {
            Some(FieldText::Selected(_)) => (true, 0, -1),
            Some(FieldText::All { value, caret }) => (
                false,
                value.chars().count() as u64,
                caret.map_or(-1, |c| c as i64),
            ),
            None => (false, 0, -1),
        };
        logging::info(
            "write.field",
            &[
                Field::Id("role", super::role_code(role)),
                Field::Flag("caret_bounds", caret_bounds),
                Field::Flag("hidden", hidden),
                Field::Id("found", way as i64),
                Field::Flag("has_text", text.is_some()),
                Field::Flag("selected", selected),
                Field::Count("value_chars", value_chars),
                Field::Id("caret_at", caret_at),
                Field::Flag("instruction", !call.instruction.is_empty()),
                Field::Id("replace", super::replace_code(call.replace)),
            ],
        );
    }

    /// The text of `front`'s focused window, read as capture reads it, with
    /// secrets redacted. `Err` if Lax may not read it (an excluded site, a
    /// pause meanwhile); `Ok(None)` if there's nothing to read.
    pub fn read_window(app: &AppHandle, front: App) -> Result<Option<String>, ()> {
        let (_, exclusions) = app.state::<Current>().newer_than(0).ok_or(())?;
        let pause = app.state::<Pause>();
        let mut screen = Pinned {
            screen: AxScreen::default(),
            app: front,
        };
        let mut capturer = Capturer::new(exclusions, Limits::default());
        match capturer.tick(&mut screen, || pause.is_paused()) {
            Outcome::Captured(snapshot) => {
                let text = format!("{}\n{}", snapshot.window_title, snapshot.text);
                Ok(Some(truncate_chars(&redact(&text), MAX_CONTEXT_CHARS)))
            }
            Outcome::Paused | Outcome::Skipped(_) => Err(()),
            _ => Ok(None),
        }
    }

    /// The screen, with the app Lax was called to held in front: by the time
    /// its window is read, Lax's own panel is.
    struct Pinned {
        screen: AxScreen,
        app: App,
    }

    impl Screen for Pinned {
        type Node = <AxScreen as Screen>::Node;

        fn frontmost_app(&mut self) -> Option<App> {
            Some(self.app.clone())
        }

        fn focused_window(&mut self, app: &App) -> Option<Self::Node> {
            self.screen.focused_window(app)
        }
    }

    /// Brings the app with this process id back to the front.
    pub(crate) fn activate(pid: i32) {
        if let Some(app) = NSRunningApplication::runningApplicationWithProcessIdentifier(pid) {
            #[allow(deprecated)] // The replacement needs macOS 14.
            let _ =
                app.activateWithOptions(NSApplicationActivationOptions::ActivateIgnoringOtherApps);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "macos")]
    use crate::ax::FieldText;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// Feeds `inputs`, each `after` the one before; whether any completed a
    /// double tap, and at which index.
    fn run(inputs: &[(u64, Input)]) -> Option<usize> {
        let mut tap = DoubleTap::default();
        let mut now = Instant::now();
        inputs.iter().position(|&(after, input)| {
            now += ms(after);
            tap.input(input, now)
        })
    }

    const DOWN: Input = Input::Modifiers(Held::KeyOnly);
    const UP: Input = Input::Modifiers(Held::Nothing);
    const OTHER: Input = Input::Modifiers(Held::Other);

    #[test]
    fn a_double_tap_calls_lax() {
        assert_eq!(run(&[(0, DOWN), (80, UP), (150, DOWN), (80, UP)]), Some(3));
    }

    #[test]
    fn other_uses_of_option_do_not() {
        // A single tap.
        assert_eq!(run(&[(0, DOWN), (80, UP)]), None);
        // ⌥2 (@) between the taps, or during the second one.
        assert_eq!(
            run(&[
                (0, DOWN),
                (80, UP),
                (50, DOWN),
                (20, Input::Pressed),
                (50, UP)
            ]),
            None
        );
        assert_eq!(
            run(&[
                (0, DOWN),
                (20, Input::Pressed),
                (50, UP),
                (100, DOWN),
                (80, UP)
            ]),
            None
        );
        // Taps too far apart, or ⌥ held too long.
        assert_eq!(run(&[(0, DOWN), (80, UP), (401, DOWN), (80, UP)]), None);
        assert_eq!(run(&[(0, DOWN), (80, UP), (100, DOWN), (501, UP)]), None);
        // ⌥ with ⇧, released in either order.
        assert_eq!(
            run(&[
                (0, DOWN),
                (80, UP),
                (100, DOWN),
                (20, OTHER),
                (20, DOWN),
                (20, UP)
            ]),
            None
        );
        // A key typed between two taps.
        assert_eq!(
            run(&[
                (0, DOWN),
                (80, UP),
                (50, Input::Pressed),
                (50, DOWN),
                (80, UP)
            ]),
            None
        );
    }

    #[test]
    fn a_late_first_tap_can_start_a_new_pair() {
        // The first tap is too old by the time ⌥ goes down again, but that
        // press can still be the first of a new pair.
        assert_eq!(
            run(&[
                (0, DOWN),
                (80, UP),
                (900, DOWN),
                (80, UP),
                (150, DOWN),
                (80, UP)
            ]),
            Some(5)
        );
        // After a double tap, the next tap starts over.
        assert_eq!(
            run(&[
                (0, DOWN),
                (80, UP),
                (100, DOWN),
                (80, UP),
                (100, DOWN),
                (80, UP)
            ]),
            Some(3)
        );
    }

    #[test]
    fn modifiers_are_read_from_the_flags() {
        let held = |flags| held(flags, CallKey::Option);
        assert_eq!(held(0), Held::Nothing);
        assert_eq!(held(0x100), Held::Nothing); // Device-dependent bits only.
        assert_eq!(held(OPTION), Held::KeyOnly);
        assert_eq!(held(OPTION | CAPS_LOCK), Held::KeyOnly);
        assert_eq!(held(OPTION | NUMERIC_PAD | 0x20), Held::KeyOnly);
        assert_eq!(held(OPTION | 1 << 17), Held::Other); // ⇧
        assert_eq!(held(1 << 20), Held::Other); // ⌘
        assert_eq!(held(CAPS_LOCK), Held::Nothing);
    }

    #[test]
    fn another_key_can_call_lax() {
        // Only the right ⌥: the left one is left to other apps.
        let right = |flags| held(flags, CallKey::RightOption);
        assert_eq!(right(OPTION | RIGHT_OPTION), Held::KeyOnly);
        assert_eq!(right(OPTION | LEFT_OPTION), Held::Other);
        assert_eq!(right(OPTION | LEFT_OPTION | RIGHT_OPTION), Held::Other);
        assert_eq!(right(0), Held::Nothing);
        // Control, Command and Shift on their own; ⌥ is then just another key.
        assert_eq!(held(CONTROL, CallKey::Control), Held::KeyOnly);
        assert_eq!(held(COMMAND, CallKey::Command), Held::KeyOnly);
        assert_eq!(held(SHIFT, CallKey::Shift), Held::KeyOnly);
        assert_eq!(held(OPTION, CallKey::Control), Held::Other);
        assert_eq!(held(CONTROL | SHIFT, CallKey::Control), Held::Other);
        // Stored by name, and an unknown name isn't a key.
        for key in [
            CallKey::Option,
            CallKey::RightOption,
            CallKey::Control,
            CallKey::Command,
            CallKey::Shift,
        ] {
            assert_eq!(CallKey::parse(key.as_str()), Some(key));
        }
        assert_eq!(CallKey::parse("capsLock"), None);
    }

    #[test]
    fn lax_never_comes_to_an_app_it_may_not_read() {
        let current = Current::default();
        current.set(Exclusions {
            apps: vec!["com.example.mail".to_string()],
            ..Exclusions::default()
        });
        let mut listener = Listener::default();
        let tap = |listener: &mut Listener, app: &str| {
            [DOWN, UP, DOWN, UP]
                .into_iter()
                .filter_map(|input| listener.input(&current, app, input))
                .count()
        };
        assert_eq!(tap(&mut listener, "com.example.mail"), 0);
        assert_eq!(tap(&mut listener, "com.1password.1password"), 0);
        assert_eq!(tap(&mut listener, ""), 0);
        assert_eq!(tap(&mut listener, "com.example.notes"), 1);

        assert!(may_come(true, false, Decision::Capture));
        assert!(!may_come(false, false, Decision::Capture));
        assert!(!may_come(true, true, Decision::Capture));
    }

    #[test]
    fn the_salmon_stops_at_the_cursor_and_the_panel_stays_on_screen() {
        let screen = Rect {
            x: 0.0,
            y: 0.0,
            width: 1440.0,
            height: 900.0,
        };
        let line = |x, y| Rect {
            x,
            y,
            width: 1.0,
            height: 18.0,
        };
        // The nose (200, 11 into the window) on the cursor, mid-line; the
        // card as wide as the field.
        assert_eq!(
            place(line(600.0, 300.0), Some(250.0), screen),
            (400.0, 298.0, 422.0)
        );
        // Within limits: never narrower than 180 or wider than 320.
        assert_eq!(place(line(600.0, 300.0), Some(90.0), screen).2, 352.0);
        assert_eq!(place(line(600.0, 300.0), Some(1200.0), screen).2, 492.0);
        assert_eq!(place(line(600.0, 300.0), None, screen).2, 432.0);
        // Near the edges the window stays on screen.
        assert_eq!(place(line(100.0, 300.0), None, screen).0, 0.0);
        assert_eq!(place(line(1430.0, 300.0), None, screen).0, 1008.0);
        assert_eq!(place(line(600.0, 880.0), None, screen).1, 800.0);
        assert_eq!(place(line(600.0, 0.0), None, screen).1, 0.0);
        // A second display to the left of the main one.
        let left = Rect {
            x: -1920.0,
            y: 0.0,
            width: 1920.0,
            height: 1080.0,
        };
        assert_eq!(
            place(line(-1000.0, 100.0), None, left),
            (-1200.0, 98.0, 432.0)
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_instruction_is_the_selection_or_the_paragraph_at_the_cursor() {
        let all = |value: &str, caret| FieldText::All {
            value: value.to_string(),
            caret,
        };
        // A note: the title, then what the user wrote, cursor at its end.
        let note = "Test från Anna\nsvara i min ton";
        assert_eq!(
            instruction_in(&all(note, Some(30))),
            (
                "svara i min ton".to_string(),
                Replace::Paragraph {
                    start: 15,
                    len: 15,
                    whole: false
                }
            )
        );
        // A one-line field is all of it.
        assert_eq!(
            instruction_in(&all("svara Anna", Some(10))),
            (
                "svara Anna".to_string(),
                Replace::Paragraph {
                    start: 0,
                    len: 10,
                    whole: true
                }
            )
        );
        // Without a cursor, all of it.
        assert_eq!(
            instruction_in(&all("svara Anna", None)).1,
            Replace::Paragraph {
                start: 0,
                len: 10,
                whole: true
            }
        );
        // A selection wins.
        assert_eq!(
            instruction_in(&FieldText::Selected("gör den kortare".to_string())),
            ("gör den kortare".to_string(), Replace::Selection)
        );
        // On an empty line, nothing: the user types in the panel.
        assert_eq!(
            instruction_in(&all("Rubrik\n", Some(7))),
            (String::new(), Replace::Nothing)
        );
    }

    #[test]
    fn paragraphs_are_counted_in_utf16_units() {
        // "🐟" is two units; "å" one.
        let text = "Lax 🐟\nåtta\u{2029}nio";
        assert_eq!(paragraph_at(text, 0), ("Lax 🐟".to_string(), 0, 6));
        assert_eq!(paragraph_at(text, 8), ("åtta".to_string(), 7, 4));
        assert_eq!(paragraph_at(text, 13), ("nio".to_string(), 12, 3));
        assert_eq!(paragraph_at(text, 15), ("nio".to_string(), 12, 3));
    }

    #[test]
    fn short_field_text_is_the_instruction() {
        assert_eq!(
            instruction_from("  skriv ett svar i min ton  "),
            "skriv ett svar i min ton"
        );
        // A whole document isn't an instruction.
        assert_eq!(instruction_from(&"Lorem ipsum. ".repeat(40)), "");
        // Secrets are redacted.
        let key = format!("sk-{}", "a1B2c3D4e5F6g7H8i9J0k1L2m3N4o5P6");
        assert!(!instruction_from(&format!("use {key}")).contains(&key));
    }

    #[test]
    fn the_field_is_logged_as_codes() {
        assert_eq!(role_code(Some("AXTextArea")), 1);
        assert_eq!(role_code(Some("AXTextView")), 9);
        assert_eq!(role_code(Some("AXSomethingNew")), 0);
        assert_eq!(role_code(None), 0);
        assert_eq!(replace_code(Replace::Nothing), 0);
        assert_eq!(
            replace_code(Replace::Paragraph {
                start: 0,
                len: 3,
                whole: true
            }),
            3
        );
    }

    #[test]
    fn a_scroll_area_is_not_a_field_to_type_in() {
        // Pages and Claude name a scroll area as focused; its top left corner
        // is not where the user types, so Lax looks inside it instead.
        assert!(typed_in(Some("AXTextArea")));
        assert!(typed_in(Some("AXTextView")));
        assert!(!typed_in(Some("AXScrollArea")));
        assert!(!typed_in(Some("AXGroup")));
        assert!(!typed_in(None));
    }

    #[test]
    fn only_a_sane_card_gets_glass() {
        let card = |x, y, width, height| Rect {
            x,
            y,
            width,
            height,
        };
        assert!(card(160.0, 26.0, 260.0, 80.0).is_sane());
        assert!(!card(f64::NAN, 26.0, 260.0, 80.0).is_sane());
        assert!(!card(160.0, 26.0, -1.0, 80.0).is_sane());
        assert!(!card(160.0, 26.0, 260.0, 1.0e9).is_sane());
    }

    #[test]
    fn the_listener_never_keeps_which_key() {
        // An input is a press or a set of held modifiers: no room for a key.
        assert!(std::mem::size_of::<Input>() <= 2);
        // And nothing in this file asks an event for its key or characters.
        let source = include_str!("summon.rs");
        for method in ["key", "characters", "charactersIgnoringModifiers"] {
            let call = format!("{method}{}", if method == "key" { "Code(" } else { "(" });
            assert!(!source.contains(&call), "summon.rs calls {call}");
        }
    }
}
