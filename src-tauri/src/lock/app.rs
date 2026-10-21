//! The lock in the running app (ADR 0012): the gate in front of every
//! command, the `lock_status` and `unlock` commands, asking when the user
//! opens the window, and the watcher that locks when the user is away.
//!
//! The gate wraps the handler `generate_handler!` makes, so every command
//! passes it before it runs, and a command added later without a rule is
//! refused ([`gate`]). Locking reloads the window's webview, so nothing it
//! showed stays in the page's memory ([`locked`]).

use std::ptr::NonNull;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde::Serialize;
use tauri::ipc::Invoke;
use tauri::{AppHandle, Emitter, Manager, Runtime};

use super::macos::LocalAuthentication;
use super::{
    Asker, Authenticator, Clock, LOCKED, Lock, Locked, MAIN_WINDOW, MacEvent, Refused, SystemClock,
};
use crate::logging::{self, Field};
use crate::store::Store;

/// The lock Lax runs with, managed as Tauri state.
pub type AppLock = Lock<LocalAuthentication>;

/// A new lock, locked. Lax makes one at start.
pub fn new() -> AppLock {
    Lock::new(LocalAuthentication, SystemClock)
}

/// Sent to the web UI when the window locks or an unlock it didn't ask for
/// ends. Must match `LOCK_CHANGED` in `src/Lock.tsx`.
pub const CHANGED_EVENT: &str = "lock-changed";

/// How often the watcher reads idle time and the window's visibility.
const TICK: Duration = Duration::from_secs(5);

/// The event's payload.
#[derive(Debug, Clone, Serialize)]
pub struct Changed {
    /// The window is locked now.
    locked: bool,
    /// What to tell the user, if anything: why an unlock failed.
    message: Option<&'static str>,
}

/// The gate (ADR 0012, "Enforced in Rust, failing closed"). Wraps the
/// handler from `generate_handler!`: each call is checked against its
/// command's rule and the window it came from ([`Lock::allows`]) before the
/// command runs, and refused with [`LOCKED`] otherwise. Without the lock in
/// Tauri's state, everything is refused.
pub fn gate<R: Runtime>(
    commands: impl Fn(Invoke<R>) -> bool + Send + Sync + 'static,
) -> impl Fn(Invoke<R>) -> bool + Send + Sync + 'static {
    move |invoke: Invoke<R>| {
        let allowed = {
            let webview = invoke.message.webview_ref();
            webview
                .try_state::<AppLock>()
                .is_some_and(|lock| lock.allows(invoke.message.command(), webview.label()))
        };
        if allowed {
            commands(invoke)
        } else {
            // Neither the command nor the window is logged: a count is
            // enough to notice a UI that calls while locked.
            logging::debug("lock.refused", &[]);
            invoke.resolver.reject(LOCKED);
            // Handled: refused, not unknown.
            true
        }
    }
}

/// What the window may show.
#[derive(Debug, Serialize)]
pub struct LockStatus {
    /// The window is locked: it shows only the lock screen.
    locked: bool,
}

#[tauri::command]
pub fn lock_status(lock: tauri::State<'_, AppLock>) -> LockStatus {
    LockStatus {
        locked: !lock.status().window_unlocked,
    }
}

/// Asks for Touch ID or the login password to unlock the window, when the
/// user pressed Unlock. A cancel is refused with no message (`null`), any
/// other failure with a short one.
#[tauri::command]
pub async fn unlock(app: AppHandle) -> Result<(), Option<&'static str>> {
    // The dialog blocks until the user answers: off the main thread and the
    // async workers.
    tauri::async_runtime::spawn_blocking(move || app.state::<AppLock>().unlock(Asker::Window))
        .await
        .unwrap_or(Err(Refused::Failed(super::Failure::Unavailable)))
        .map_err(Refused::message)
}

/// `work`'s output, or `None` once the window is locked (checked every
/// second, as a pause is). The gate checks a command when it starts; this
/// keeps one that takes long, such as Ask's answer, from handing what Lax
/// learned to a window that locked meanwhile, and stops what it streams.
pub async fn unless_locked<T>(app: &AppHandle, work: impl Future<Output = T>) -> Option<T> {
    until_locked(&app.state::<AppLock>(), LOCK_CHECK, work).await
}

/// How often [`unless_locked`] looks at the lock.
const LOCK_CHECK: Duration = Duration::from_secs(1);

async fn until_locked<A: Authenticator, C: Clock, T>(
    lock: &Lock<A, C>,
    every: Duration,
    work: impl Future<Output = T>,
) -> Option<T> {
    crate::pause::until(|| !lock.status().window_unlocked, every, work).await
}

/// The window is about to be shown. Applies the hidden-window rule first,
/// so a window hidden for 5 minutes locks, and is reloaded, before it
/// shows anything. Never asks.
pub fn showing(app: &AppHandle) {
    if let Some(what) = app.state::<AppLock>().window_visible(true) {
        locked(app, what);
    }
}

/// The window was hidden: the hidden-window rule counts from now. Never
/// asks.
pub fn hidden(app: &AppHandle) {
    if let Some(what) = app.state::<AppLock>().window_visible(false) {
        locked(app, what);
    }
}

/// The user opened the window from the menu bar: if it is locked, ask at
/// once (ADR 0012, "It asks at once when the window is shown"). Only once
/// the database is open: before, the window shows where opening stands, and
/// a Keychain prompt may be next. The answer goes to the window as an event.
pub fn ask_on_open(app: &AppHandle) {
    if app.state::<AppLock>().status().window_unlocked || app.try_state::<Mutex<Store>>().is_none()
    {
        return;
    }
    let app = app.clone();
    // The dialog blocks until the user answers, as in `unlock`.
    tauri::async_runtime::spawn_blocking(move || {
        let lock = app.state::<AppLock>();
        let result = lock.unlock(Asker::Window);
        let _ = app.emit(
            CHANGED_EVENT,
            Changed {
                locked: !lock.status().window_unlocked,
                message: result.err().and_then(Refused::message),
            },
        );
    });
}

/// Lax or the window locked: tell the web UI, and reload the window so
/// nothing it showed stays in the page's memory. Anything typed and not
/// saved is lost (ADR 0012).
fn locked(app: &AppHandle, what: Locked) {
    logging::info(
        "lock.locked",
        &[Field::Flag("window_only", what == Locked::Window)],
    );
    let _ = app.emit(
        CHANGED_EVENT,
        Changed {
            locked: true,
            message: None,
        },
    );
    if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
        let _ = window.reload();
    }
}

/// Starts the watcher: a thread that every few seconds reads how long since
/// the last input and whether the window is visible, and the Mac's
/// notifications for screen lock, sleep and account switch. Runs whether
/// capture runs or not. Call on the main thread.
pub fn watch(app: &AppHandle) {
    let handle = app.clone();
    thread::spawn(move || {
        loop {
            thread::sleep(TICK);
            let (idle, visible) = (crate::ax::idle_time(), window_visible(&handle));
            if let Some(what) = handle.state::<AppLock>().tick(idle, visible) {
                locked(&handle, what);
            }
        }
    });

    let handle = app.clone();
    notifications::observe(move |which, event| {
        if let Some(what) = on_notification(&handle.state::<AppLock>(), which, event) {
            locked(&handle, what);
        }
    });
}

/// One of the Mac's notifications, `which` in [`NOTIFICATIONS`].
fn on_notification<A: Authenticator, C: Clock>(
    lock: &Lock<A, C>,
    which: usize,
    event: MacEvent,
) -> Option<Locked> {
    logging::info("lock.notification", &[Field::Count("which", which as u64)]);
    lock.mac_event(event)
}

/// Whether the main window is on screen: shown and not minimised. A window
/// whose state can't be read counts as hidden, which can only lock it
/// sooner.
fn window_visible(app: &AppHandle) -> bool {
    app.get_webview_window(MAIN_WINDOW).is_some_and(|window| {
        window.is_visible().unwrap_or(false) && !window.is_minimized().unwrap_or(true)
    })
}

/// The notifications that lock Lax at once, by name (ADR 0012, "Away").
/// The log numbers them by their place here, so the maintainer can see
/// which fire, and when.
const NOTIFICATIONS: [(&str, MacEvent); 4] = [
    ("NSWorkspaceWillSleepNotification", MacEvent::Slept),
    ("NSWorkspaceScreensDidSleepNotification", MacEvent::Slept),
    (
        "NSWorkspaceSessionDidResignActiveNotification",
        MacEvent::SwitchedAccount,
    ),
    (SCREEN_LOCKED, MacEvent::ScreenLocked),
];

/// Posted by loginwindow, through the distributed notification center, when
/// the screen locks. Undocumented.
const SCREEN_LOCKED: &str = "com.apple.screenIsLocked";

/// The place in [`NOTIFICATIONS`] of the one named `name`, and its event.
fn event_for(name: &str) -> Option<(usize, MacEvent)> {
    NOTIFICATIONS
        .iter()
        .position(|(known, _)| *known == name)
        .map(|which| (which, NOTIFICATIONS[which].1))
}

#[allow(unsafe_code)] // Notification observers; see SAFETY below.
mod notifications {
    //! The Mac's notifications that lock Lax at once: `NSWorkspace`'s
    //! `willSleep`, `screensDidSleep` and `sessionDidResignActive`, and the
    //! distributed `com.apple.screenIsLocked`, which is undocumented.

    use super::*;

    use block2::RcBlock;
    use objc2_app_kit::{
        NSWorkspace, NSWorkspaceScreensDidSleepNotification,
        NSWorkspaceSessionDidResignActiveNotification, NSWorkspaceWillSleepNotification,
    };
    use objc2_foundation::{
        NSDistributedNotificationCenter, NSNotification, NSNotificationCenter, NSString,
    };

    type OnEvent = Arc<dyn Fn(usize, MacEvent) + Send + Sync>;

    /// Calls `on_event` with each of [`NOTIFICATIONS`] and its place there,
    /// on the thread that posts it, for as long as Lax runs.
    pub fn observe(on_event: impl Fn(usize, MacEvent) + Send + Sync + 'static) {
        let on_event: OnEvent = Arc::new(on_event);
        let workspace = NSWorkspace::sharedWorkspace().notificationCenter();
        // SAFETY: AppKit's constant notification names, never changed.
        let names = unsafe {
            [
                NSWorkspaceWillSleepNotification,
                NSWorkspaceScreensDidSleepNotification,
                NSWorkspaceSessionDidResignActiveNotification,
            ]
        };
        for name in names {
            add(&workspace, name, &on_event);
        }
        let distributed = NSDistributedNotificationCenter::defaultCenter();
        add(&distributed, &NSString::from_str(SCREEN_LOCKED), &on_event);
    }

    fn add(center: &NSNotificationCenter, name: &NSString, on_event: &OnEvent) {
        let Some((which, event)) = event_for(&name.to_string()) else {
            return;
        };
        let on_event = Arc::clone(on_event);
        let block = RcBlock::new(move |_: NonNull<NSNotification>| on_event(which, event));
        // SAFETY: no object to match and no queue, so the block runs on the
        // posting thread. The block is sendable, as the method requires: it
        // holds only an `Arc` of a `Send + Sync` closure and a plain enum.
        // The center keeps the observer until it is removed, which Lax
        // never does, so the returned token may be dropped.
        let _token = unsafe {
            center.addObserverForName_object_queue_usingBlock(Some(name), None, None, &block)
        };
    }
}

#[cfg(test)]
mod tests {
    use super::super::fake::{FakeAuthenticator, FakeClock};
    use super::super::{AWAY, Language, Outcome, PANEL_WINDOW, Status};
    use super::*;

    fn unlocked() -> Lock<FakeAuthenticator, FakeClock> {
        let mut lock = Lock::new(
            FakeAuthenticator::answering(&[Outcome::Success]),
            FakeClock::default(),
        );
        lock.language = || Language::English;
        lock.window_visible(true);
        lock.unlock(Asker::Window).unwrap();
        lock
    }

    const LOCKED_STATUS: Status = Status {
        unlocked: false,
        window_unlocked: false,
    };

    #[test]
    fn the_watcher_locks_after_five_minutes_without_input() {
        let lock = unlocked();
        let mut idle = Duration::ZERO;
        let mut first = None;
        while idle < AWAY {
            lock.clock.advance(TICK);
            idle += TICK;
            let seen = lock.tick(Some(idle), true);
            assert!(first.is_none() || seen.is_none(), "locked twice");
            first = first.or(seen);
        }
        assert_eq!(first, Some(Locked::Lax));
        assert_eq!(lock.status(), LOCKED_STATUS);
        // Nothing asked.
        assert_eq!(lock.authenticator.requests(), 1);
    }

    #[test]
    fn the_watcher_locks_the_window_hidden_for_five_minutes() {
        let lock = unlocked();
        let mut waited = Duration::ZERO;
        let mut first = None;
        while waited <= AWAY {
            lock.clock.advance(TICK);
            waited += TICK;
            // The user types elsewhere, with Lax's window closed.
            first = first.or(lock.tick(Some(Duration::from_secs(1)), false));
        }
        assert_eq!(first, Some(Locked::Window));
        assert!(lock.status().unlocked);
        assert!(!lock.status().window_unlocked);
    }

    #[test]
    fn each_notification_locks_at_once() {
        for name in [
            "com.apple.screenIsLocked",
            "NSWorkspaceWillSleepNotification",
            "NSWorkspaceScreensDidSleepNotification",
            "NSWorkspaceSessionDidResignActiveNotification",
        ] {
            let (which, event) = event_for(name).expect(name);
            let lock = unlocked();
            assert_eq!(
                on_notification(&lock, which, event),
                Some(Locked::Lax),
                "{name}"
            );
            assert_eq!(lock.status(), LOCKED_STATUS, "{name}");
            assert_eq!(lock.authenticator.requests(), 1, "{name}");
        }
        assert_eq!(
            event_for("com.apple.screenIsLocked").map(|(_, event)| event),
            Some(MacEvent::ScreenLocked)
        );
        assert_eq!(event_for("NSWorkspaceDidWakeNotification"), None);
        assert_eq!(event_for("com.apple.screenIsUnlocked"), None);
    }

    /// The names the watcher matches are AppKit's own.
    #[test]
    #[allow(unsafe_code)]
    fn the_notification_names_are_appkits() {
        use objc2_app_kit::{
            NSWorkspaceScreensDidSleepNotification, NSWorkspaceSessionDidResignActiveNotification,
            NSWorkspaceWillSleepNotification,
        };
        // SAFETY: AppKit's constant notification names, never changed.
        let names = unsafe {
            [
                (NSWorkspaceWillSleepNotification, MacEvent::Slept),
                (NSWorkspaceScreensDidSleepNotification, MacEvent::Slept),
                (
                    NSWorkspaceSessionDidResignActiveNotification,
                    MacEvent::SwitchedAccount,
                ),
            ]
        };
        for (name, event) in names {
            let found = event_for(&name.to_string()).map(|(_, event)| event);
            assert_eq!(found, Some(event), "{name}");
        }
    }

    #[tokio::test]
    async fn work_stops_when_the_window_locks() {
        let check = Duration::from_millis(10);
        let lock = unlocked();
        assert_eq!(until_locked(&lock, check, async { 7 }).await, Some(7));
        // Locks while the work is under way, as Ask's answer streams.
        let stopped = until_locked(&lock, check, async {
            lock.mac_event(MacEvent::ScreenLocked);
            std::future::pending::<()>().await
        });
        assert_eq!(stopped.await, None);
        // Locked: nothing runs to its end.
        assert_eq!(until_locked(&lock, check, async { 7 }).await, None);

        // Only the window locked: stopped too.
        let lock = unlocked();
        lock.window_visible(false);
        lock.clock.advance(AWAY);
        lock.window_visible(true);
        assert!(lock.status().unlocked);
        assert_eq!(until_locked(&lock, check, async { 7 }).await, None);
    }

    // The gate itself, wrapping real invokes on Tauri's mock runtime.

    /// An app whose only handler, behind the gate, answers with the name of
    /// the command it was asked to run: the gate goes by name.
    fn mock_app(with_lock: bool) -> tauri::App<tauri::test::MockRuntime> {
        let builder = tauri::test::mock_builder();
        let builder = if with_lock {
            builder.manage(new())
        } else {
            builder
        };
        builder
            .invoke_handler(gate(|invoke: Invoke<tauri::test::MockRuntime>| {
                let ran = invoke.message.command().to_string();
                invoke.resolver.resolve(ran);
                true
            }))
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("the mock app")
    }

    fn window(
        app: &tauri::App<tauri::test::MockRuntime>,
        label: &str,
    ) -> tauri::WebviewWindow<tauri::test::MockRuntime> {
        tauri::WebviewWindowBuilder::new(app, label, Default::default())
            .build()
            .expect("the window")
    }

    /// What `command` answers, called from `window`.
    fn call(
        window: &tauri::WebviewWindow<tauri::test::MockRuntime>,
        command: &str,
    ) -> Result<String, serde_json::Value> {
        tauri::test::get_ipc_response(
            window,
            tauri::webview::InvokeRequest {
                cmd: command.into(),
                callback: tauri::ipc::CallbackFn(0),
                error: tauri::ipc::CallbackFn(1),
                url: "tauri://localhost".parse().expect("the url"),
                body: tauri::ipc::InvokeBody::default(),
                headers: Default::default(),
                invoke_key: tauri::test::INVOKE_KEY.to_string(),
            },
        )
        .map(|body| body.deserialize::<String>().expect("a string"))
    }

    fn refused() -> Result<String, serde_json::Value> {
        Err(serde_json::Value::String(LOCKED.to_string()))
    }

    #[test]
    fn the_gate_refuses_before_the_command_runs() {
        let app = mock_app(true);
        let (main, panel) = (window(&app, MAIN_WINDOW), window(&app, PANEL_WINDOW));

        // Locked: only what the lock screen needs runs.
        assert_eq!(call(&main, "recent_captures"), refused());
        assert_eq!(call(&main, "database_status"), Ok("database_status".into()));
        assert_eq!(call(&main, "not_in_any_list"), refused());
        // The panel's commands from the panel only.
        assert_eq!(call(&panel, "write_draft"), Ok("write_draft".into()));
        assert_eq!(call(&main, "write_draft"), refused());
        assert_eq!(call(&panel, "database_status"), refused());

        // Unlocked: the window's commands run, and a command without a rule
        // is still refused.
        let lock = app.state::<AppLock>();
        lock.state().unlock(Asker::Window, lock.clock.now());
        assert_eq!(call(&main, "recent_captures"), Ok("recent_captures".into()));
        assert_eq!(call(&main, "not_in_any_list"), refused());
        assert_eq!(call(&panel, "recent_captures"), refused());

        // Locked again.
        lock.mac_event(MacEvent::ScreenLocked);
        assert_eq!(call(&main, "recent_captures"), refused());
    }

    #[test]
    fn without_the_lock_the_gate_refuses_everything() {
        let app = mock_app(false);
        let (main, panel) = (window(&app, MAIN_WINDOW), window(&app, PANEL_WINDOW));
        assert_eq!(call(&main, "database_status"), refused());
        assert_eq!(call(&main, "recent_captures"), refused());
        assert_eq!(call(&panel, "write_draft"), refused());
    }
}
