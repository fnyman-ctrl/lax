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
    CFArray, CFBoolean, CFGetTypeID, CFIndex, CFRange, CFRetained, CFString, CFType, CFTypeID,
    CFURL, kCFBooleanTrue,
};

use crate::capture::{App, Node, Screen, truncate_chars};

type AXError = i32;
const AX_SUCCESS: AXError = 0;
/// `kAXValueCFRangeType`.
const AX_VALUE_CF_RANGE: u32 = 4;

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
        }
    }
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
}
