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
    /// ⌥ Option and nothing else.
    OptionOnly,
    /// Anything else, such as ⌘ or ⇧, alone or with ⌥.
    Other,
}

/// macOS modifier flags (`NSEventModifierFlags`).
const CAPS_LOCK: usize = 1 << 16;
const OPTION: usize = 1 << 19;
const NUMERIC_PAD: usize = 1 << 21;
const DEVICE_INDEPENDENT: usize = 0xffff_0000;

/// Which modifiers `flags` (an event's modifier flags) say are held.
pub fn held(flags: usize) -> Held {
    // Caps Lock is a state, not a key held; the numeric pad flag comes with
    // arrow keys. Neither keeps ⌥ from being alone.
    match flags & DEVICE_INDEPENDENT & !(CAPS_LOCK | NUMERIC_PAD) {
        0 => Held::Nothing,
        OPTION => Held::OptionOnly,
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
            (Held::Nothing, Held::OptionOnly) => {
                if self
                    .tapped
                    .is_some_and(|end| now.saturating_duration_since(end) > GAP)
                {
                    self.tapped = None;
                }
                self.down = Some(now);
                false
            }
            (Held::OptionOnly, Held::Nothing) => {
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

/// The app that was in front when Lax was called, to give focus back to.
#[derive(Default)]
pub struct Summoned(Mutex<Option<i32>>);

/// The panel's window label. Its capability is `capabilities/panel.json`.
pub const PANEL: &str = "panel";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WritingStatus {
    enabled: bool,
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
    Ok(WritingStatus { enabled })
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
    Ok(WritingStatus { enabled })
}

/// Closes the panel and gives focus back to the app Lax was called to.
#[tauri::command]
pub fn close_panel(app: AppHandle) {
    if let Some(panel) = app.get_webview_window(PANEL) {
        let _ = panel.hide();
    }
    let pid = app
        .state::<Summoned>()
        .0
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take();
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

/// A rectangle on screen, in points, from the top left of the main display.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[repr(C)] // Read from an AXValue as a CGRect.
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// The panel window: room for the salmon to swim in from the left, and the
/// card. Matches `.panel` in `src/styles.css`.
const PANEL_WIDTH: f64 = 500.0;
const PANEL_HEIGHT: f64 = 190.0;
/// How far right of the window's left edge the salmon stops, which is put
/// at the text cursor.
const SALMON_STOPS: f64 = 150.0;

/// Where the panel's top left goes for the text cursor at `caret`: just
/// below the line, the salmon stopping at the cursor, all of it on `screen`.
/// Above the line if there's no room below.
fn place(caret: Rect, screen: Rect) -> (f64, f64) {
    let mut y = caret.y + caret.height + 2.0;
    if y + PANEL_HEIGHT > screen.y + screen.height {
        y = caret.y - PANEL_HEIGHT - 2.0;
    }
    let x = caret.x - SALMON_STOPS;
    let right = (screen.x + screen.width - PANEL_WIDTH).max(screen.x);
    let bottom = (screen.y + screen.height - PANEL_HEIGHT).max(screen.y);
    (x.clamp(screen.x, right), y.clamp(screen.y, bottom))
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
        if let Some((pid, caret)) = macos::locate(&app) {
            show(&app, pid, caret);
        }
        #[cfg(not(target_os = "macos"))]
        let _ = app;
    });
}

/// Shows the panel at `caret`, or by the mouse pointer if the app didn't say
/// where its text cursor is.
fn show(app: &AppHandle, pid: i32, caret: Option<Rect>) {
    logging::info(
        "write.summoned",
        &[Field::Flag("at_cursor", caret.is_some())],
    );
    *app.state::<Summoned>()
        .0
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = Some(pid);
    let panel = match app.get_webview_window(PANEL) {
        Some(panel) => panel,
        None => {
            let built = tauri::WebviewWindowBuilder::new(
                app,
                PANEL,
                tauri::WebviewUrl::App("index.html".into()),
            )
            .title("Lax")
            .inner_size(PANEL_WIDTH, PANEL_HEIGHT)
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
        x: caret.x - PANEL_WIDTH,
        y: caret.y - PANEL_HEIGHT,
        width: PANEL_WIDTH * 3.0,
        height: PANEL_HEIGHT * 3.0,
    });
    let (x, y) = place(caret, screen);
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
mod macos {
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

    use super::{Input, Listener, Rect, come, held};
    use crate::ax::AxScreen;
    use crate::capture::{Limits, Node, admit};
    use crate::exclusions::{Current, is_secure_text_field};
    use crate::pause::Pause;

    thread_local! {
        /// The installed monitor. AppKit objects stay on the main thread.
        static MONITOR: RefCell<Option<Retained<AnyObject>>> = const { RefCell::new(None) };
    }

    /// The only place that sees an event. It keeps whether it was a press
    /// or a change of modifiers, and which modifiers; never the key.
    fn reduce(event: &NSEvent) -> Input {
        if event.r#type() == NSEventType::FlagsChanged {
            Input::Modifiers(held(event.modifierFlags().0))
        } else {
            Input::Pressed
        }
    }

    pub fn install(app: AppHandle) {
        remove();
        let listener = RefCell::new(Listener::default());
        let block = RcBlock::new(move |event: NonNull<NSEvent>| {
            // SAFETY: AppKit passes a valid event for the duration of the call.
            let input = reduce(unsafe { event.as_ref() });
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

    /// Where the text cursor is in the app in front, after the same checks
    /// capture makes before reading a window: the app, the window title and
    /// whether a browser window is private. `None` if Lax may not come: one
    /// of those says no, capture is paused, or the focus is in a password
    /// field. Only positions are read, never text.
    pub fn locate(app: &AppHandle) -> Option<(i32, Option<Rect>)> {
        // Before the user's exclusions are loaded, Lax comes nowhere.
        let (_, exclusions) = app.state::<Current>().newer_than(0)?;
        let pause = app.state::<Pause>();
        let mut screen = AxScreen::default();
        let (front, _window, _title) = admit(&mut screen, &exclusions, &Limits::default(), &|| {
            pause.is_paused()
        })
        .ok()?;
        let Some(element) = screen.focused_element(&front) else {
            return Some((front.pid, None));
        };
        let (role, subrole) = element.role_and_subrole();
        if is_secure_text_field(role.as_deref().unwrap_or_default(), subrole.as_deref()) {
            return None;
        }
        Some((front.pid, element.caret()))
    }

    /// Brings the app with this process id back to the front.
    pub fn activate(pid: i32) {
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

    const DOWN: Input = Input::Modifiers(Held::OptionOnly);
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
        assert_eq!(held(0), Held::Nothing);
        assert_eq!(held(0x100), Held::Nothing); // Device-dependent bits only.
        assert_eq!(held(OPTION), Held::OptionOnly);
        assert_eq!(held(OPTION | CAPS_LOCK), Held::OptionOnly);
        assert_eq!(held(OPTION | NUMERIC_PAD | 0x20), Held::OptionOnly);
        assert_eq!(held(OPTION | 1 << 17), Held::Other); // ⇧
        assert_eq!(held(1 << 20), Held::Other); // ⌘
        assert_eq!(held(CAPS_LOCK), Held::Nothing);
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
    fn the_panel_goes_by_the_cursor_and_stays_on_screen() {
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
        // Below the line, the salmon stopping at the cursor.
        assert_eq!(place(line(600.0, 300.0), screen), (450.0, 320.0));
        // Near the left edge, the window stays on screen.
        assert_eq!(place(line(20.0, 300.0), screen), (0.0, 320.0));
        // Near the right edge too.
        assert_eq!(place(line(1430.0, 300.0), screen), (940.0, 320.0));
        // No room below: above the line.
        assert_eq!(place(line(600.0, 850.0), screen), (450.0, 658.0));
        // A second display to the left of the main one.
        let left = Rect {
            x: -1920.0,
            y: 0.0,
            width: 1920.0,
            height: 1080.0,
        };
        assert_eq!(place(line(-1000.0, 100.0), left), (-1150.0, 120.0));
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
