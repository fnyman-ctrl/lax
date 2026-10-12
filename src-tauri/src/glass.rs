//! Liquid Glass behind the writing assistant's card (macOS 26 and later):
//! a native `NSGlassEffectView` under the transparent web view, at the
//! card's place, so what is behind the panel shows through, blurred. On
//! older macOS there is no glass and the card keeps its own background.
//!
//! Only on the main thread, as AppKit wants.

use tauri::WebviewWindow;

use crate::summon::Rect;

/// Puts the glass behind `card` (in points, from the window's top left), or
/// hides it for `None`. Returns whether this Mac has the glass at all.
pub fn set(panel: &WebviewWindow, card: Option<Rect>) -> bool {
    #[cfg(target_os = "macos")]
    {
        if !macos::available() {
            return false;
        }
        let Ok(window) = panel.ns_window() else {
            return false;
        };
        // A pointer can't cross threads; its address can, and the window
        // outlives the call (it is only ever hidden, never closed).
        let window = window as usize;
        let _ = panel.run_on_main_thread(move || macos::set(window, card));
        true
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (panel, card);
        false
    }
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)] // The window pointer Tauri hands out; see SAFETY.
mod macos {
    use std::cell::RefCell;

    use objc2::MainThreadMarker;
    use objc2::rc::Retained;
    use objc2::runtime::AnyClass;
    use objc2_app_kit::{NSGlassEffectView, NSView, NSWindow, NSWindowOrderingMode};
    use objc2_foundation::{NSPoint, NSRect, NSSize};

    use crate::summon::Rect;

    /// The card's corner radius in `src/styles.css`.
    const CORNER_RADIUS: f64 = 10.0;

    thread_local! {
        /// The glass view, once made. AppKit objects stay on the main thread.
        static GLASS: RefCell<Option<Retained<NSView>>> = const { RefCell::new(None) };
    }

    /// Whether this macOS has Liquid Glass (macOS 26 and later).
    pub fn available() -> bool {
        AnyClass::get(c"NSGlassEffectView").is_some()
    }

    pub fn set(window: usize, card: Option<Rect>) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        // SAFETY: `window` is the address of the panel's NSWindow, which
        // Tauri keeps alive as long as the panel exists; we are on the main
        // thread, where AppKit objects may be used.
        let window: &NSWindow = unsafe { &*(window as *const NSWindow) };
        let Some(content) = window.contentView() else {
            return;
        };
        GLASS.with(|slot| {
            let mut slot = slot.borrow_mut();
            let Some(card) = card else {
                if let Some(glass) = slot.as_ref() {
                    glass.setHidden(true);
                }
                return;
            };
            let glass = slot.get_or_insert_with(|| {
                let glass = NSGlassEffectView::new(mtm);
                glass.setCornerRadius(CORNER_RADIUS);
                let glass: Retained<NSView> = Retained::into_super(glass);
                // Behind everything in the window: the web view draws on top.
                content.addSubview_positioned_relativeTo(&glass, NSWindowOrderingMode::Below, None);
                glass
            });
            let height = content.bounds().size.height;
            let y = if content.isFlipped() {
                card.y
            } else {
                height - card.y - card.height
            };
            glass.setFrame(NSRect::new(
                NSPoint::new(card.x, y),
                NSSize::new(card.width, card.height),
            ));
            glass.setHidden(false);
        });
    }
}
