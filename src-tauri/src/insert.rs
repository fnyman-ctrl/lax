//! Putting a draft into the field the user called Lax to (ADR 0009, step 4).
//!
//! Only when the user presses Enter on a draft they have read, only into the
//! field Lax was called to, and only text: Lax never presses Return or Send
//! in the other app (GUARDRAILS §5). Through Accessibility where the app
//! takes it; otherwise pasted, with the clipboard marked concealed and
//! transient for clipboard managers, and put back as it was.

use std::time::Duration;

use tauri::{AppHandle, Manager};

use crate::logging::{self, ErrorKind, Field};
use crate::pause::Pause;
use crate::summon::{PANEL, Summoned};
use crate::write::check_draft;

/// How long the app may take to come back to the front.
const BACK_IN_FRONT: Duration = Duration::from_millis(1500);

/// Puts `text` (the draft, perhaps edited by the user) into the field Lax
/// was called to, and closes the panel.
#[tauri::command]
pub async fn insert_draft(app: AppHandle, text: String) -> Result<(), String> {
    check_draft(&text).map_err(|_| "The draft can't be inserted as it is.".to_string())?;
    if app.state::<Pause>().is_paused() {
        return Err("Lax is paused. Resume it from the menu bar to insert.".to_string());
    }
    let Some(call) = app.state::<Summoned>().forget() else {
        return Err("Call Lax to the text field again.".to_string());
    };
    if let Some(panel) = app.get_webview_window(PANEL) {
        let _ = panel.hide();
    }
    let text = text.trim().to_string();
    let handle = app.clone();
    let kept = call.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        #[cfg(target_os = "macos")]
        {
            macos::insert(&handle, &call, &text)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (handle, call, text);
            Err(Failed::NotInFront)
        }
    })
    .await
    .unwrap_or(Err(Failed::NotInFront));
    // Nothing was written: the panel comes back with the draft, so it isn't
    // lost and can be copied.
    if result.is_err() {
        app.state::<Summoned>().set(Some(kept));
        if let Some(panel) = app.get_webview_window(PANEL) {
            let _ = panel.show();
            let _ = panel.set_focus();
        }
    }
    match result {
        Ok(how) => {
            logging::info(
                "write.inserted",
                &[Field::Flag("accessibility", how == How::Accessibility)],
            );
            Ok(())
        }
        Err(failed) => {
            logging::warn(
                "write.insert_failed",
                &[
                    Field::Flag("not_in_front", failed == Failed::NotInFront),
                    Field::Error(ErrorKind::Other),
                ],
            );
            Err(if failed == Failed::NotInFront {
                "Lax couldn't get back to the field. Select the draft and copy it with ⌘C."
            } else {
                "Lax won't write into that field. Select the draft and copy it with ⌘C."
            }
            .to_string())
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum How {
    Accessibility,
    Pasted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Failed {
    /// The app didn't come back to the front, or isn't one Lax may write to.
    NotInFront,
    /// The field is a password field, or Lax was paused meanwhile.
    Refused,
}

/// Whether writing through Accessibility worked: the field's text changed.
/// Some apps accept the request and do nothing.
fn written(before: &str, after: Option<&str>) -> bool {
    after.is_some_and(|after| after != before)
}

/// Whether `len` UTF-16 units of `value` from `start` are still the paragraph
/// the instruction was taken from.
fn still_there(value: &str, start: usize, len: usize, instruction: &str) -> bool {
    let units: Vec<u16> = value.encode_utf16().collect();
    let Some(paragraph) = units.get(start..start.saturating_add(len)) else {
        return false;
    };
    crate::summon::instruction_from(&String::from_utf16_lossy(paragraph)) == instruction
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)] // CoreGraphics key events; see SAFETY below.
mod macos {
    use std::ffi::c_void;
    use std::thread;
    use std::time::{Duration, Instant};

    use objc2::rc::{Retained, autoreleasepool};
    use objc2::runtime::ProtocolObject;
    use objc2_app_kit::{NSPasteboard, NSPasteboardItem, NSWorkspace};
    use objc2_core_foundation::{CFRetained, CFType};
    use objc2_foundation::{NSArray, NSData, NSString};
    use tauri::{AppHandle, Manager};

    use super::{BACK_IN_FRONT, Failed, How, still_there, written};
    use crate::ax::AxScreen;
    use crate::capture::{App, Node};
    use crate::exclusions::{Current, Decision, is_secure_text_field};
    use crate::pause::Pause;
    use crate::summon::{Call, Replace};

    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        // Returns a new event the caller owns ("Create" rule), or null.
        fn CGEventCreateKeyboardEvent(
            source: *const c_void,
            keycode: u16,
            key_down: bool,
        ) -> *mut CFType;
        fn CGEventSetFlags(event: &CFType, flags: u64);
        fn CGEventPost(tap: u32, event: &CFType);
    }

    /// `kCGHIDEventTap`, `kCGEventFlagMaskCommand`.
    const HID_EVENT_TAP: u32 = 0;
    const COMMAND: u64 = 1 << 20;
    /// Key codes of A and V (`kVK_ANSI_A`, `kVK_ANSI_V`): ⌘A and ⌘V, the only
    /// keys Lax ever sends. Never Return.
    const KEY_A: u16 = 0x00;
    const KEY_V: u16 = 0x09;

    /// How long the app gets to read the clipboard after ⌘V before it is put
    /// back as it was.
    const PASTE_READ: Duration = Duration::from_millis(500);

    pub fn insert(app: &AppHandle, call: &Call, text: &str) -> Result<How, Failed> {
        let pid = call.pid;
        let _ = app.run_on_main_thread(move || crate::summon::macos::activate(pid));
        let started = Instant::now();
        let bundle_id = loop {
            if let Some((front, bundle_id)) = frontmost()
                && front == pid
            {
                break bundle_id;
            }
            if started.elapsed() > BACK_IN_FRONT {
                return Err(Failed::NotInFront);
            }
            thread::sleep(Duration::from_millis(30));
        };
        // Let the app put the keyboard focus back in its field.
        thread::sleep(Duration::from_millis(80));
        // The same rules as when Lax was called: never into an app Lax may
        // not read, nor while paused.
        let allowed = app
            .state::<Current>()
            .newer_than(0)
            .is_some_and(|(_, exclusions)| exclusions.check_app(&bundle_id) == Decision::Capture);
        if !allowed {
            return Err(Failed::NotInFront);
        }
        if app.state::<Pause>().is_paused() {
            return Err(Failed::Refused);
        }
        let screen = AxScreen::default();
        let front = App {
            pid,
            bundle_id,
            name: String::new(),
        };
        let element = screen.focused_element(&front);
        if let Some(element) = &element {
            let (role, subrole) = element.role_and_subrole();
            if is_secure_text_field(role.as_deref().unwrap_or_default(), subrole.as_deref()) {
                return Err(Failed::Refused);
            }
        }
        // Whether the instruction is selected now, ready to be replaced.
        let mut selected = !matches!(call.replace, Replace::Paragraph { .. });
        if call.accessible
            && let Some(element) = &element
            && let Some(before) = element.value()
        {
            // Only if the paragraph still holds the instruction: the field
            // may have changed while the panel was open.
            if let Replace::Paragraph { start, len, .. } = call.replace
                && still_there(&before, start, len, &call.instruction)
            {
                selected = element.select(start, len);
            }
            if selected
                && element.replace_selection(text)
                && written(&before, element.value().as_deref())
            {
                return Ok(How::Accessibility);
            }
        }
        // ⌘A only when the instruction was the whole field and couldn't be
        // selected otherwise. If a paragraph couldn't be selected, the draft
        // goes in at the cursor and the instruction stays for the user.
        let whole = matches!(call.replace, Replace::Paragraph { whole: true, .. });
        paste(text, whole && !selected);
        Ok(How::Pasted)
    }

    /// The app in front: its process id and bundle identifier.
    fn frontmost() -> Option<(i32, String)> {
        autoreleasepool(|_| {
            let app = NSWorkspace::sharedWorkspace().frontmostApplication()?;
            Some((app.processIdentifier(), app.bundleIdentifier()?.to_string()))
        })
    }

    /// Pastes `text` with ⌘V, after ⌘A if `select_all`, through the
    /// clipboard, which is put back as it was unless something else was
    /// copied meanwhile.
    fn paste(text: &str, select_all: bool) {
        autoreleasepool(|_| {
            let board = NSPasteboard::generalPasteboard();
            let saved = save(&board);
            board.clearContents();
            let item = NSPasteboardItem::new();
            item.setString_forType(
                &NSString::from_str(text),
                &NSString::from_str("public.utf8-plain-text"),
            );
            // Clipboard managers that honour these (nspasteboard.org) keep
            // the draft out of their history.
            let empty = NSData::new();
            for marker in [
                "org.nspasteboard.ConcealedType",
                "org.nspasteboard.TransientType",
            ] {
                item.setData_forType(&empty, &NSString::from_str(marker));
            }
            let items = NSArray::from_retained_slice(&[ProtocolObject::from_retained(item)]);
            board.writeObjects(&items);
            let ours = board.changeCount();
            if select_all {
                command_key(KEY_A);
                thread::sleep(Duration::from_millis(60));
            }
            command_key(KEY_V);
            thread::sleep(PASTE_READ);
            if board.changeCount() == ours {
                restore(&board, saved);
            }
        });
    }

    /// Every item on the clipboard, as type and data.
    type Saved = Vec<Vec<(Retained<NSString>, Retained<NSData>)>>;

    fn save(board: &NSPasteboard) -> Saved {
        let Some(items) = board.pasteboardItems() else {
            return Vec::new();
        };
        items
            .iter()
            .map(|item| {
                item.types()
                    .iter()
                    .filter_map(|kind| item.dataForType(&kind).map(|data| (kind.clone(), data)))
                    .collect()
            })
            .collect()
    }

    fn restore(board: &NSPasteboard, saved: Saved) {
        board.clearContents();
        if saved.is_empty() {
            return;
        }
        let items: Vec<_> = saved
            .into_iter()
            .map(|types| {
                let item = NSPasteboardItem::new();
                for (kind, data) in types {
                    item.setData_forType(&data, &kind);
                }
                ProtocolObject::from_retained(item)
            })
            .collect();
        board.writeObjects(&NSArray::from_retained_slice(&items));
    }

    /// Presses and releases ⌘ and `key`.
    fn command_key(key: u16) {
        for down in [true, false] {
            // SAFETY: a null source is allowed; returns a new event or null.
            let event = unsafe { CGEventCreateKeyboardEvent(std::ptr::null(), key, down) };
            let Some(event) = std::ptr::NonNull::new(event) else {
                return;
            };
            // SAFETY: the event is a new reference we own.
            let event: CFRetained<CFType> = unsafe { CFRetained::from_raw(event) };
            // SAFETY: a valid event and plain values.
            unsafe {
                CGEventSetFlags(&event, COMMAND);
                CGEventPost(HID_EVENT_TAP, &event);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writing_counts_only_when_the_text_changed() {
        assert!(written("skriv ett svar", Some("Hej Anna!")));
        assert!(!written("skriv ett svar", Some("skriv ett svar")));
        assert!(!written("skriv ett svar", None));
    }

    #[test]
    fn a_paragraph_is_replaced_only_if_it_still_holds_the_instruction() {
        let note = "Test från Anna\nsvara i min ton";
        assert!(still_there(note, 15, 15, "svara i min ton"));
        // Text arrived above it meanwhile.
        assert!(!still_there(
            &format!("Ny rad\n{note}"),
            15,
            15,
            "svara i min ton"
        ));
        // Past the end.
        assert!(!still_there(note, 15, 40, "svara i min ton"));
    }

    #[test]
    fn lax_never_presses_return() {
        // Only ⌘A and ⌘V are ever sent; Return is key code 0x24, Enter 0x4C.
        let source = include_str!("insert.rs");
        let sent: Vec<&str> = source
            .lines()
            .filter(|line| line.trim_start().starts_with("const KEY_"))
            .collect();
        assert_eq!(sent.len(), 2, "{sent:?}");
        for line in sent {
            assert!(!line.contains("0x24") && !line.contains("0x4C"), "{line}");
        }
    }
}
