//! Reading other apps' windows through the macOS Accessibility API, for
//! capture (GUARDRAILS §4).
//!
//! This is the only module that talks to other apps, and it holds all the
//! foreign code for it. It reads only what `capture.rs` asks for through the
//! [`Screen`] and [`Node`] traits, one attribute at a time, so the order of
//! the checks there is the order of reading here.

// Calls into ApplicationServices; each one has a SAFETY comment.
#![allow(unsafe_code)]

use std::collections::HashSet;
use std::ffi::c_void;
use std::ptr::{self, NonNull};
use std::rc::Rc;
use std::time::Duration;

use objc2::rc::autoreleasepool;
use objc2_app_kit::NSWorkspace;
use objc2_core_foundation::{
    CFArray, CFBoolean, CFDictionary, CFGetTypeID, CFIndex, CFNumber, CFRange, CFRetained,
    CFString, CFType, CFTypeID, CFURL, kCFBooleanTrue,
};

use crate::capture::{App, Node, Screen, truncate_chars};
use crate::summon::Rect;

type AXError = i32;
const AX_SUCCESS: AXError = 0;
/// `kAXValueCGPointType`, `kAXValueCGSizeType`, `kAXValueCGRectType` and
/// `kAXValueCFRangeType`.
const AX_VALUE_CG_POINT: u32 = 1;
const AX_VALUE_CG_SIZE: u32 = 2;
const AX_VALUE_CG_RECT: u32 = 3;
const AX_VALUE_CF_RANGE: u32 = 4;

/// The height of a text field's first line, roughly, in points.
const FIRST_LINE: f64 = 24.0;

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    // SAFETY: these take no arguments and return a type identifier.
    safe fn AXUIElementGetTypeID() -> CFTypeID;
    safe fn AXValueGetTypeID() -> CFTypeID;

    // Both return a new element the caller owns ("Create" rule), or null.
    fn AXUIElementCreateSystemWide() -> *mut CFType;
    fn AXUIElementCreateApplication(pid: i32) -> *mut CFType;

    // On success, `value` holds a new reference the caller owns ("Copy" rule).
    fn AXUIElementCopyAttributeValue(
        element: &CFType,
        attribute: &CFString,
        value: *mut *const CFType,
    ) -> AXError;
    fn AXUIElementCopyAttributeValues(
        element: &CFType,
        attribute: &CFString,
        index: CFIndex,
        max_values: CFIndex,
        values: *mut *const CFArray,
    ) -> AXError;
    fn AXUIElementCopyMultipleAttributeValues(
        element: &CFType,
        attributes: &CFArray,
        options: u32,
        values: *mut *const CFArray,
    ) -> AXError;
    fn AXUIElementCopyParameterizedAttributeValue(
        element: &CFType,
        attribute: &CFString,
        parameter: &CFType,
        value: *mut *const CFType,
    ) -> AXError;

    fn AXUIElementSetAttributeValue(
        element: &CFType,
        attribute: &CFString,
        value: &CFType,
    ) -> AXError;
    fn AXUIElementSetMessagingTimeout(element: &CFType, timeout_seconds: f32) -> AXError;

    fn AXValueCreate(value_type: u32, value: *const c_void) -> *mut CFType;
    fn AXValueGetValue(value: &CFType, value_type: u32, out: *mut c_void) -> u8;
}

// CoreGraphics is part of ApplicationServices.
#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    // SAFETY: plain values in and out; reads only how long ago the last
    // input event of any kind happened, never which event it was.
    safe fn CGEventSourceSecondsSinceLastEventType(source_state: i32, event_type: u32) -> f64;
}

// CoreGraphics' window list: where windows are and which process owns them.
#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    // Returns a new array the caller owns ("Copy" rule), or null.
    fn CGWindowListCopyWindowInfo(option: u32, relative_to_window: u32) -> *const CFArray;
    // Fills `rect` from a window's bounds dictionary; false if it isn't one.
    fn CGRectMakeWithDictionaryRepresentation(dict: &CFDictionary, rect: *mut Rect) -> bool;
}

/// `kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements`.
const ON_SCREEN_WINDOWS: u32 = (1 << 0) | (1 << 4);

/// The process whose ordinary window is on top at `x`, `y` (in points).
/// Reads only where windows are, their level and which process owns them,
/// never their titles or what they show (that would need Screen Recording).
pub fn window_owner_at(x: f64, y: f64) -> Option<i32> {
    // SAFETY: plain values in; returns a new array or null.
    let windows = unsafe { CGWindowListCopyWindowInfo(ON_SCREEN_WINDOWS, 0) };
    // SAFETY: the array is a new reference we own; its items are
    // dictionaries, checked below before use.
    let windows = unsafe { owned(windows.cast()) }?
        .downcast::<CFArray>()
        .ok()?;
    let (bounds, owner, layer) = (
        CFString::from_static_str("kCGWindowBounds"),
        CFString::from_static_str("kCGWindowOwnerPID"),
        CFString::from_static_str("kCGWindowLayer"),
    );
    // Front to back.
    for index in 0..windows.count() {
        // SAFETY: `index` is within the array.
        let item = unsafe { windows.value_at_index(index) };
        let Some(item) = NonNull::new(item.cast_mut()) else {
            continue;
        };
        // SAFETY: a valid CF object, borrowed from the array.
        let item: &CFType = unsafe { item.cast().as_ref() };
        let Some(window) = item.downcast_ref::<CFDictionary>() else {
            continue;
        };
        let number = |key: &CFString| {
            // SAFETY: valid dictionary and key; the value is borrowed.
            let value = unsafe { window.value((key as *const CFString).cast()) };
            let value: &CFType = unsafe { NonNull::new(value.cast_mut())?.cast().as_ref() };
            value.downcast_ref::<CFNumber>()?.as_i64()
        };
        // Menu bar, Dock, floating panels: not the app's windows.
        if number(&layer) != Some(0) {
            continue;
        }
        // SAFETY: valid dictionary and key; the value is borrowed.
        let frame = unsafe { window.value((&*bounds as *const CFString).cast()) };
        let Some(frame) = NonNull::new(frame.cast_mut()) else {
            continue;
        };
        // SAFETY: a valid CF object, borrowed from the dictionary.
        let frame: &CFType = unsafe { frame.cast().as_ref() };
        let Some(frame) = frame.downcast_ref::<CFDictionary>() else {
            continue;
        };
        let mut rect = Rect::default();
        // SAFETY: valid dictionary; `rect` is a CGRect-shaped out value.
        if !unsafe { CGRectMakeWithDictionaryRepresentation(frame, &mut rect) } {
            continue;
        }
        if x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height {
            return number(&owner).and_then(|pid| i32::try_from(pid).ok());
        }
    }
    None
}

/// `kCGEventSourceStateCombinedSessionState`.
const COMBINED_SESSION_STATE: i32 = 0;
/// `kCGAnyInputEventType`.
const ANY_INPUT_EVENT: u32 = u32::MAX;

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    // Returns a new snapshot the caller owns ("Copy" rule), or null.
    fn IOPSCopyPowerSourcesInfo() -> *mut CFType;
    // Returns a string owned by the snapshot ("Get" rule), or null.
    fn IOPSGetProvidingPowerSourceType(snapshot: &CFType) -> *const CFString;
}

/// How long since the user last pressed a key, moved the mouse, scrolled or
/// clicked. Only the time is read, never what the input was, and macOS asks
/// for no permission for it.
pub fn idle_time() -> Option<Duration> {
    let seconds = CGEventSourceSecondsSinceLastEventType(COMBINED_SESSION_STATE, ANY_INPUT_EVENT);
    (seconds.is_finite() && seconds >= 0.0).then(|| Duration::from_secs_f64(seconds))
}

/// Whether the Mac runs on battery right now. `None` if it can't be read.
pub fn on_battery() -> Option<bool> {
    // SAFETY: no arguments; returns a new snapshot or null.
    let snapshot = unsafe { owned(IOPSCopyPowerSourcesInfo()) }?;
    // SAFETY: `snapshot` is a valid power source snapshot; the string is
    // borrowed from it and retained before the snapshot goes away.
    let source = NonNull::new(unsafe { IOPSGetProvidingPowerSourceType(&snapshot) }.cast_mut())?;
    let source = unsafe { CFRetained::retain(source) };
    // kIOPMBatteryPowerKey
    Some(source.to_string() == "Battery Power")
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    // Returns the URL's string without passing ownership ("Get" rule), or null.
    fn CFURLGetString(url: &CFURL) -> *const CFString;
}

/// The attribute names Lax reads, made once instead of on every read.
struct Names {
    role: CFRetained<CFString>,
    subrole: CFRetained<CFString>,
    identifier: CFRetained<CFString>,
    title: CFRetained<CFString>,
    description: CFRetained<CFString>,
    url: CFRetained<CFString>,
    value: CFRetained<CFString>,
    children: CFRetained<CFString>,
    focused_window: CFRetained<CFString>,
    manual_accessibility: CFRetained<CFString>,
    visible_character_range: CFRetained<CFString>,
    string_for_range: CFRetained<CFString>,
    focused_element: CFRetained<CFString>,
    selected_text: CFRetained<CFString>,
    selected_text_range: CFRetained<CFString>,
    bounds_for_range: CFRetained<CFString>,
    position: CFRetained<CFString>,
    size: CFRetained<CFString>,
    /// AXRole and AXSubrole, for reading both in one request.
    role_and_subrole: CFRetained<CFArray<CFString>>,
}

impl Names {
    fn new() -> Self {
        let name = CFString::from_static_str;
        Names {
            role: name("AXRole"),
            subrole: name("AXSubrole"),
            identifier: name("AXIdentifier"),
            title: name("AXTitle"),
            description: name("AXDescription"),
            url: name("AXURL"),
            value: name("AXValue"),
            children: name("AXChildren"),
            focused_window: name("AXFocusedWindow"),
            manual_accessibility: name("AXManualAccessibility"),
            visible_character_range: name("AXVisibleCharacterRange"),
            string_for_range: name("AXStringForRange"),
            focused_element: name("AXFocusedUIElement"),
            selected_text: name("AXSelectedText"),
            selected_text_range: name("AXSelectedTextRange"),
            bounds_for_range: name("AXBoundsForRange"),
            position: name("AXPosition"),
            size: name("AXSize"),
            role_and_subrole: CFArray::from_retained_objects(&[name("AXRole"), name("AXSubrole")]),
        }
    }
}

/// The value inside an AXValue of `value_type`. `T` must be the matching
/// plain C type: `Rect` for a rectangle, `[f64; 2]` for a point or a size.
fn ax_value<T: Default>(value: &CFType, value_type: u32) -> Option<T> {
    if CFGetTypeID(Some(value)) != AXValueGetTypeID() {
        return None;
    }
    let mut out = T::default();
    // SAFETY: `value` is an AXValue; the callers pass the C type matching
    // `value_type`, so the framework writes a value of the right size, and
    // only if the value is of that type.
    let ok = unsafe { AXValueGetValue(value, value_type, (&raw mut out).cast::<c_void>()) };
    (ok != 0).then_some(out)
}

/// Takes ownership of a reference returned by a "Create" or "Copy" function.
///
/// # Safety
///
/// `ptr` must be null or a valid CoreFoundation object the caller owns.
unsafe fn owned(ptr: *const CFType) -> Option<CFRetained<CFType>> {
    // SAFETY: guaranteed by the caller; CFRetained releases it once.
    NonNull::new(ptr.cast_mut()).map(|ptr| unsafe { CFRetained::from_raw(ptr) })
}

/// How long any app may take to answer one request before Lax gives up, so
/// a hung app can't block capture. Set on the system-wide element, which
/// makes it the default for every element.
pub fn set_messaging_timeout(timeout: Duration) {
    // SAFETY: no arguments; returns a new element or null.
    let Some(system) = (unsafe { owned(AXUIElementCreateSystemWide()) }) else {
        return;
    };
    // SAFETY: `system` is a valid element.
    unsafe { AXUIElementSetMessagingTimeout(&system, timeout.as_secs_f32()) };
}

/// The real screen, read through the Accessibility API. Lives on the capture
/// thread.
pub struct AxScreen {
    names: Rc<Names>,
    /// Processes already asked to expose their accessibility tree.
    prepared: HashSet<i32>,
}

impl Default for AxScreen {
    fn default() -> Self {
        AxScreen {
            names: Rc::new(Names::new()),
            prepared: HashSet::new(),
        }
    }
}

/// Stop remembering prepared processes beyond this many; preparing a process
/// twice is harmless.
const MAX_PREPARED: usize = 512;

impl AxScreen {
    /// The element of `app` that has keyboard focus, such as the text field
    /// being written in. Only called once the app and window were checked.
    pub fn focused_element(&self, app: &App) -> Option<Element> {
        // SAFETY: any pid is accepted; returns a new element or null.
        let application = Element {
            element: unsafe { owned(AXUIElementCreateApplication(app.pid)) }?,
            names: self.names.clone(),
        };
        application.element(&self.names.focused_element)
    }
}

impl Screen for AxScreen {
    type Node = Element;

    fn frontmost_app(&mut self) -> Option<App> {
        // A thread of its own has no autorelease pool; AppKit needs one.
        autoreleasepool(|_| {
            let app = NSWorkspace::sharedWorkspace().frontmostApplication()?;
            Some(App {
                pid: app.processIdentifier(),
                bundle_id: app
                    .bundleIdentifier()
                    .map(|id| id.to_string())
                    .unwrap_or_default(),
                name: app
                    .localizedName()
                    .map(|name| name.to_string())
                    .unwrap_or_default(),
            })
        })
    }

    fn focused_window(&mut self, app: &App) -> Option<Element> {
        // SAFETY: any pid is accepted; returns a new element or null.
        let element = Element {
            element: unsafe { owned(AXUIElementCreateApplication(app.pid)) }?,
            names: self.names.clone(),
        };
        if self.prepared.len() >= MAX_PREPARED {
            self.prepared.clear();
        }
        if self.prepared.insert(app.pid) {
            // Chromium and Electron apps (Chrome, Slack, VS Code…) only build
            // their accessibility tree when asked. Asking is expensive for
            // them, so it's done once per process. Other apps ignore it.
            element.set_true(&self.names.manual_accessibility);
        }
        element.element(&self.names.focused_window)
    }
}

/// An element of another app's window.
pub struct Element {
    element: CFRetained<CFType>,
    names: Rc<Names>,
}

impl Element {
    fn copy(&self, attribute: &CFString) -> Option<CFRetained<CFType>> {
        let mut value: *const CFType = ptr::null();
        // SAFETY: `self.element` is an AXUIElement (checked when it was
        // made), `attribute` a valid string and `value` a valid out pointer.
        let error = unsafe { AXUIElementCopyAttributeValue(&self.element, attribute, &mut value) };
        if error != AX_SUCCESS {
            return None;
        }
        // SAFETY: on success the value is a new reference we own.
        unsafe { owned(value) }
    }

    fn string(&self, attribute: &CFString) -> Option<String> {
        let value = self.copy(attribute)?.downcast::<CFString>().ok()?;
        Some(value.to_string())
    }

    fn element(&self, attribute: &CFString) -> Option<Element> {
        self.child(self.copy(attribute)?)
    }

    /// `value` as an element, if it is one.
    fn child(&self, value: CFRetained<CFType>) -> Option<Element> {
        (CFGetTypeID(Some(&value)) == AXUIElementGetTypeID()).then(|| Element {
            element: value,
            names: self.names.clone(),
        })
    }

    fn set_true(&self, attribute: &CFString) {
        // SAFETY: reading a static the framework provides.
        let Some(yes) = (unsafe { kCFBooleanTrue }) else {
            return;
        };
        let yes: &CFBoolean = yes;
        // SAFETY: valid element, attribute and value. A failure (the app
        // doesn't support it) is ignored.
        unsafe { AXUIElementSetAttributeValue(&self.element, attribute, yes.as_ref()) };
    }

    /// Where the text cursor is, or the element's first line where the app
    /// can't say, so the salmon doesn't end up below a whole document.
    /// Reads positions only, never text.
    pub fn caret(&self) -> Option<Rect> {
        self.caret_bounds().or_else(|| {
            self.frame().map(|frame| Rect {
                height: frame.height.min(FIRST_LINE),
                ..frame
            })
        })
    }

    fn caret_bounds(&self) -> Option<Rect> {
        let range = self.copy(&self.names.selected_text_range)?;
        if CFGetTypeID(Some(&range)) != AXValueGetTypeID() {
            return None;
        }
        let mut value: *const CFType = ptr::null();
        // SAFETY: valid element, attribute, parameter (the AXValue range the
        // app gave) and out pointer.
        let error = unsafe {
            AXUIElementCopyParameterizedAttributeValue(
                &self.element,
                &self.names.bounds_for_range,
                &range,
                &mut value,
            )
        };
        if error != AX_SUCCESS {
            return None;
        }
        // SAFETY: on success the value is a new reference we own.
        let bounds = unsafe { owned(value) }?;
        let rect: Rect = ax_value(&bounds, AX_VALUE_CG_RECT)?;
        // Some apps answer an empty selection with an empty rectangle at 0,0.
        (rect.height > 0.0 && (rect.x != 0.0 || rect.y != 0.0)).then_some(rect)
    }

    /// The text the user selected in a field, or else all of its text, at
    /// most `max_chars` characters. Only called for the field the user
    /// called Lax to, once it is known not to be a password field.
    pub fn field_text(&self, max_chars: usize) -> Option<String> {
        let selected = self
            .string(&self.names.selected_text)
            .filter(|text| !text.trim().is_empty());
        let text = selected.or_else(|| self.string(&self.names.value))?;
        Some(truncate_chars(&text, max_chars))
    }

    /// Where the element is on screen.
    pub fn frame(&self) -> Option<Rect> {
        let position = self.copy(&self.names.position)?;
        let size = self.copy(&self.names.size)?;
        let position: [f64; 2] = ax_value(&position, AX_VALUE_CG_POINT)?;
        let size: [f64; 2] = ax_value(&size, AX_VALUE_CG_SIZE)?;
        Some(Rect {
            x: position[0],
            y: position[1],
            width: size[0],
            height: size[1],
        })
    }

    /// The text currently visible in a text area, which can be far shorter
    /// than its whole value (a long document).
    fn visible_text(&self, max_chars: usize) -> Option<String> {
        let range_value = self.copy(&self.names.visible_character_range)?;
        if CFGetTypeID(Some(&range_value)) != AXValueGetTypeID() {
            return None;
        }
        let mut range = CFRange::new(0, 0);
        // SAFETY: `range_value` is an AXValue; `range` is a CFRange, the type
        // asked for, so the framework writes a value of the right size.
        let ok = unsafe {
            AXValueGetValue(
                &range_value,
                AX_VALUE_CF_RANGE,
                (&raw mut range).cast::<c_void>(),
            )
        };
        if ok == 0 || range.length <= 0 {
            return None;
        }
        // Lengths are in UTF-16 units; two per character is always enough.
        let max_units = CFIndex::try_from(max_chars.saturating_mul(2)).unwrap_or(CFIndex::MAX);
        range.length = range.length.min(max_units);
        // SAFETY: `range` is a CFRange, matching the type passed; returns a
        // new value or null.
        let parameter = unsafe {
            owned(AXValueCreate(
                AX_VALUE_CF_RANGE,
                (&raw const range).cast::<c_void>(),
            ))
        }?;
        let mut value: *const CFType = ptr::null();
        // SAFETY: valid element, attribute, parameter and out pointer.
        let error = unsafe {
            AXUIElementCopyParameterizedAttributeValue(
                &self.element,
                &self.names.string_for_range,
                &parameter,
                &mut value,
            )
        };
        if error != AX_SUCCESS {
            return None;
        }
        // SAFETY: on success the value is a new reference we own.
        let text = unsafe { owned(value) }?.downcast::<CFString>().ok()?;
        Some(truncate_chars(&text.to_string(), max_chars))
    }
}

impl Node for Element {
    fn role(&self) -> Option<String> {
        self.string(&self.names.role)
    }

    fn subrole(&self) -> Option<String> {
        self.string(&self.names.subrole)
    }

    fn role_and_subrole(&self) -> (Option<String>, Option<String>) {
        let mut values: *const CFArray = ptr::null();
        let attributes: &CFArray = self.names.role_and_subrole.as_ref();
        // SAFETY: valid element, an array of attribute name strings and an
        // out pointer. With options 0, an attribute the element lacks gives
        // an error value in its place instead of failing the whole call.
        let error = unsafe {
            AXUIElementCopyMultipleAttributeValues(&self.element, attributes, 0, &mut values)
        };
        // Some apps can't answer the combined request; ask one at a time.
        let one_at_a_time = || (self.role(), self.subrole());
        if error != AX_SUCCESS {
            return one_at_a_time();
        }
        // SAFETY: on success the array is a new reference we own.
        let Some(array) = (unsafe { owned(values.cast()) }) else {
            return one_at_a_time();
        };
        let Ok(array) = array.downcast::<CFArray>() else {
            return one_at_a_time();
        };
        // SAFETY: the array holds CoreFoundation objects; each is checked to
        // be a string below, and error values are not.
        let array = unsafe { CFRetained::cast_unchecked::<CFArray<CFType>>(array) };
        let string = |i: usize| {
            array
                .get(i)
                .and_then(|value| value.downcast::<CFString>().ok())
                .map(|s| s.to_string())
        };
        (string(0), string(1))
    }

    fn identifier(&self) -> Option<String> {
        self.string(&self.names.identifier)
    }

    fn title(&self) -> Option<String> {
        self.string(&self.names.title)
    }

    fn description(&self) -> Option<String> {
        self.string(&self.names.description)
    }

    fn url(&self) -> Option<String> {
        let value = self.copy(&self.names.url)?;
        match value.downcast::<CFURL>() {
            Ok(url) => {
                // SAFETY: `url` is a valid CFURL. The string is borrowed from
                // it, so it is retained before `url` goes away. (CFURL::string
                // would panic instead of returning nothing, and a panic quits
                // Lax.)
                let string = NonNull::new(unsafe { CFURLGetString(&url) }.cast_mut())?;
                let string = unsafe { CFRetained::retain(string) };
                Some(string.to_string())
            }
            Err(value) => value.downcast::<CFString>().ok().map(|s| s.to_string()),
        }
    }

    fn text(&self, role: &str, max_chars: usize) -> Option<String> {
        if role == "AXTextArea"
            && let Some(text) = self.visible_text(max_chars)
        {
            return Some(text);
        }
        Some(truncate_chars(&self.string(&self.names.value)?, max_chars))
    }

    fn children(&self, max: usize) -> Vec<Element> {
        let mut values: *const CFArray = ptr::null();
        let max = CFIndex::try_from(max).unwrap_or(CFIndex::MAX);
        // SAFETY: valid element, attribute and out pointer; at most `max`
        // values are returned.
        let error = unsafe {
            AXUIElementCopyAttributeValues(&self.element, &self.names.children, 0, max, &mut values)
        };
        if error != AX_SUCCESS {
            return Vec::new();
        }
        // SAFETY: on success the array is a new reference we own.
        let Some(array) = (unsafe { owned(values.cast()) }) else {
            return Vec::new();
        };
        let Ok(array) = array.downcast::<CFArray>() else {
            return Vec::new();
        };
        // SAFETY: an array from CoreFoundation only holds CoreFoundation
        // objects; each one's type is checked below.
        let array = unsafe { CFRetained::cast_unchecked::<CFArray<CFType>>(array) };
        array.iter().filter_map(|value| self.child(value)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_time_and_power_source_can_be_read() {
        // Reads only system state that needs no permission, so this runs on
        // any Mac, including CI. The values themselves depend on the machine.
        if let Some(idle) = idle_time() {
            assert!(idle < Duration::from_secs(10 * 365 * 24 * 3600));
        }
        let _ = on_battery();
    }

    #[test]
    fn the_window_on_top_can_be_found() {
        // Needs no permission either. Far off any screen there's no window;
        // on the main screen there may be one, owned by a real process.
        assert_eq!(window_owner_at(-1.0e7, -1.0e7), None);
        if let Some(pid) = window_owner_at(400.0, 400.0) {
            assert!(pid > 0);
        }
    }
}
