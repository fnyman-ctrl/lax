//! The app lock (ADR 0012, GUARDRAILS §6).
//!
//! Lax's window shows nothing until the user confirms with Touch ID or the
//! login password, through macOS's LocalAuthentication, and locks again when
//! the user is away. This module holds the lock's state, the away rules, the
//! reasons the dialog shows, the [`Authenticator`] that asks, and each
//! command's lock rule ([`rule`], [`Lock::allows`]). [`app`] puts it into the
//! running app: the gate in front of the commands, `lock_status` and
//! `unlock`, and the watcher that feeds [`Lock::tick`] and
//! [`Lock::mac_event`].
//!
//! The rules this module keeps:
//!
//! - **Lax starts locked.** [`Lock::new`] is locked, and nothing here is
//!   stored, so nothing persists "unlocked" across a restart.
//! - **Only a success unlocks.** A cancel, a failure, an error or an answer
//!   that never comes leaves Lax locked ([`Outcome`]). A cancel says nothing
//!   ([`Refused::Cancelled`]); other failures say briefly what happened.
//! - **A new context for every request.** A used `LAContext` answers a second
//!   request with success at once, without asking (the ADR's spike). So each
//!   request makes its own context, and drops it when the answer is in; no
//!   context outlives its request, so every lock drops it. A lock that comes
//!   while a request is still asking throws that request's answer away.
//! - **Away locks.** Five minutes without input, by either clock
//!   ([`Stamp::passed`]), or at once when the screen locks, the Mac sleeps or
//!   the user switches accounts, locks Lax: the window and the writing
//!   assistant's use of the pages. Five minutes with the window hidden locks
//!   only the window ([`Lock::tick`], [`Lock::window_visible`]).
//! - **Nothing here asks on its own.** Only [`Lock::unlock`] and the
//!   confirmations ([`Lock::confirm`], [`Lock::confirm_while_locked`]) ask,
//!   and they are for something the user just did in Lax. Locking never
//!   asks, also when the window is visible. One request asks at a time;
//!   another made meanwhile is cancelled without asking.
//! - **Some commands ask again, every time.** Forgets, exports and changes
//!   that loosen privacy ask before they change anything, in Rust, even in an
//!   unlocked window ([`EVERY_TIME`], [`LOOSENING`], [`ASKS_WHILE_LOCKED`],
//!   [`Lock::confirmed`], [`Lock::confirmed_if_loosening`]). There is no
//!   token or "confirmed" state the web UI could reuse.
//! - **Default deny.** A command without a rule is refused, locked or not
//!   ([`Lock::allows`]).

use std::sync::{Mutex, MutexGuard, TryLockError};
use std::time::{Duration, Instant, SystemTime};

#[cfg(target_os = "macos")]
pub mod app;

/// How long the user may be away before Lax locks. Fixed (ADR 0012,
/// question 9).
pub const AWAY: Duration = Duration::from_secs(5 * 60);

/// A moment on both clocks. The monotonic clock can't be set back but
/// stands still while the Mac sleeps; the wall clock runs during sleep but
/// can be set back. The away rules use both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stamp {
    pub mono: Instant,
    pub wall: SystemTime,
}

impl Stamp {
    pub fn now() -> Self {
        Stamp {
            mono: Instant::now(),
            wall: SystemTime::now(),
        }
    }

    /// Whether `span` has passed between `self` and `now` on either clock.
    /// A wall clock set back counts as no time, so it never extends the
    /// time; the monotonic clock still counts.
    pub fn passed(&self, now: Stamp, span: Duration) -> bool {
        now.mono.saturating_duration_since(self.mono) >= span
            || now
                .wall
                .duration_since(self.wall)
                .is_ok_and(|since| since >= span)
    }

    /// `span` before `self` on both clocks, if both can go back that far.
    fn before(&self, span: Duration) -> Option<Stamp> {
        Some(Stamp {
            mono: self.mono.checked_sub(span)?,
            wall: self.wall.checked_sub(span)?,
        })
    }
}

/// Where [`Lock`] reads the time. Tests use `fake::FakeClock`.
pub trait Clock: Send + Sync {
    fn now(&self) -> Stamp;
}

/// The Mac's own clocks.
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Stamp {
        Stamp::now()
    }
}

/// Something the Mac did that locks Lax at once (ADR 0012, question 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacEvent {
    ScreenLocked,
    Slept,
    SwitchedAccount,
}

/// Which part of Lax asks to be unlocked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Asker {
    /// The main window. Unlocking it unlocks Lax and the window.
    Window,
    /// The writing assistant's panel. Unlocking it unlocks Lax, so drafts
    /// may use the memory pages, but not the window, which still asks when
    /// it is next shown.
    Panel,
}

/// What locked, returned so the caller can reload the window and tell the
/// web UI. Only returned when it was unlocked before.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Locked {
    /// Lax locked, and with it the window.
    Lax,
    /// Only the window locked; Lax stays unlocked for the panel.
    Window,
}

/// What is unlocked right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Status {
    /// Lax is unlocked: the writing assistant may use the memory pages.
    pub unlocked: bool,
    /// The window is unlocked too, so it may show what Lax captured and
    /// learned. Never true while Lax is locked.
    pub window_unlocked: bool,
}

/// The language the dialog's reasons are written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    English,
    Swedish,
}

impl Language {
    /// The language a BCP 47 tag such as `sv-SE` or `en-GB` names: Swedish
    /// for `sv` in any region, English for everything else.
    pub fn from_tag(tag: &str) -> Language {
        let primary = tag.split(['-', '_']).next().unwrap_or_default();
        if primary.eq_ignore_ascii_case("sv") {
            Language::Swedish
        } else {
            Language::English
        }
    }

    /// The user's first preferred language, which macOS shows the rest of
    /// the dialog in. English if it can't be read.
    pub fn system() -> Language {
        #[cfg(target_os = "macos")]
        {
            macos::first_preferred_language()
                .map(|tag| Language::from_tag(&tag))
                .unwrap_or(Language::English)
        }
        #[cfg(not(target_os = "macos"))]
        {
            Language::English
        }
    }
}

/// Everything that asks for Touch ID or the login password (ADR 0012,
/// "What else asks").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Unlocking the window.
    OpenWindow,
    /// Unlocking from the writing assistant's panel.
    UseMemoryInDraft,
    ForgetCaptures,
    DeleteAllData,
    ForgetEntity,
    ForgetMemory,
    DeleteOutbox,
    ExportPages,
    DeleteOldPageFiles,
    StartOver,
    DeleteOldDatabase,
    RemoveExclusion,
    AllowLearning,
    SetRetention,
    SetOutboxRetention,
    SetOwnNames,
    TurnOffDefaultExclusion,
    UseModelOffThisMac,
    TurnOnJournal,
    TurnOnMemory,
    TurnOnWriting,
}

impl Action {
    pub const ALL: [Action; 21] = [
        Action::OpenWindow,
        Action::UseMemoryInDraft,
        Action::ForgetCaptures,
        Action::DeleteAllData,
        Action::ForgetEntity,
        Action::ForgetMemory,
        Action::DeleteOutbox,
        Action::ExportPages,
        Action::DeleteOldPageFiles,
        Action::StartOver,
        Action::DeleteOldDatabase,
        Action::RemoveExclusion,
        Action::AllowLearning,
        Action::SetRetention,
        Action::SetOutboxRetention,
        Action::SetOwnNames,
        Action::TurnOffDefaultExclusion,
        Action::UseModelOffThisMac,
        Action::TurnOnJournal,
        Action::TurnOnMemory,
        Action::TurnOnWriting,
    ];

    /// What the dialog says Lax is trying to do. macOS puts it into its own
    /// sentence, "<App> is trying to <reason>." ("<App> försöker att
    /// <reason>."), so each is a lowercase verb phrase with no full stop.
    /// A fixed string, never anything Lax captured.
    pub fn reason(self, language: Language) -> &'static str {
        use Language::{English as En, Swedish as Sv};
        match (self, language) {
            (Action::OpenWindow, En) => "open your memory",
            (Action::OpenWindow, Sv) => "öppna ditt minne",
            (Action::UseMemoryInDraft, En) => "use your memory in a draft",
            (Action::UseMemoryInDraft, Sv) => "använda ditt minne i ett utkast",
            (Action::ForgetCaptures, En) => "forget some of what it captured",
            (Action::ForgetCaptures, Sv) => "glömma en del av det den har fångat",
            (Action::DeleteAllData, En) => "delete everything it captured and learned",
            (Action::DeleteAllData, Sv) => "radera allt den har fångat och lärt sig",
            (Action::ForgetEntity, En) => "forget a name and what it learned about it",
            (Action::ForgetEntity, Sv) => "glömma ett namn och det den lärt sig om det",
            (Action::ForgetMemory, En) => "forget everything the memory noted",
            (Action::ForgetMemory, Sv) => "glömma allt som minnet har noterat",
            (Action::DeleteOutbox, En) => "delete from the log of what was sent",
            (Action::DeleteOutbox, Sv) => "radera ur loggen över det som skickats",
            (Action::ExportPages, En) => "export your memory pages",
            (Action::ExportPages, Sv) => "exportera dina minnessidor",
            (Action::DeleteOldPageFiles, En) => "delete the old page files",
            (Action::DeleteOldPageFiles, Sv) => "radera de gamla sidfilerna",
            (Action::StartOver, En) => "start over with an empty database",
            (Action::StartOver, Sv) => "börja om med en tom databas",
            (Action::DeleteOldDatabase, En) => "delete the old database file",
            (Action::DeleteOldDatabase, Sv) => "radera den gamla databasfilen",
            (Action::RemoveExclusion, En) => "remove one of your exclusions",
            (Action::RemoveExclusion, Sv) => "ta bort ett av dina undantag",
            (Action::AllowLearning, En) => "learn a name you told it not to learn",
            (Action::AllowLearning, Sv) => "lära sig ett namn du sagt att den inte ska lära sig",
            (Action::SetRetention, En) => "change how long captures are kept",
            (Action::SetRetention, Sv) => "ändra hur länge det den fångat sparas",
            (Action::SetOutboxRetention, En) => "change how long the log of what was sent is kept",
            (Action::SetOutboxRetention, Sv) => {
                "ändra hur länge loggen över det som skickats sparas"
            }
            (Action::SetOwnNames, En) => "change your other names",
            (Action::SetOwnNames, Sv) => "ändra dina andra namn",
            (Action::TurnOffDefaultExclusion, En) => {
                "stop excluding a banking or health app or site"
            }
            (Action::TurnOffDefaultExclusion, Sv) => {
                "sluta undanta en app eller webbplats för bank eller hälsa"
            }
            (Action::UseModelOffThisMac, En) => "use a model that doesn't run on this Mac",
            (Action::UseModelOffThisMac, Sv) => "använda en modell som inte körs på den här Macen",
            (Action::TurnOnJournal, En) => "turn on the journal",
            (Action::TurnOnJournal, Sv) => "slå på journalen",
            (Action::TurnOnMemory, En) => "turn on the memory",
            (Action::TurnOnMemory, Sv) => "slå på minnet",
            (Action::TurnOnWriting, En) => "turn on the writing assistant",
            (Action::TurnOnWriting, Sv) => "slå på skrivhjälpen",
        }
    }
}

/// Why asking failed. Lax stays locked in every case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// The finger or password wasn't accepted, or there were too many tries.
    NotAccepted,
    /// macOS closed the dialog before the user answered.
    Interrupted,
    /// The Mac has no login password, so there is nothing to ask for
    /// (ADR 0012, question 7).
    NoPassword,
    /// LocalAuthentication couldn't ask, or answered with something Lax
    /// doesn't know.
    Unavailable,
}

impl Failure {
    /// A short, fixed message for the lock screen.
    pub fn message(self) -> &'static str {
        match self {
            Failure::NotAccepted => "Touch ID or the password wasn't accepted.",
            Failure::Interrupted => "macOS closed the dialog. Try again.",
            Failure::NoPassword => {
                "This Mac has no login password, so Lax can't be unlocked. Set one in System Settings."
            }
            Failure::Unavailable => "Lax couldn't ask for Touch ID or your password.",
        }
    }
}

/// The answer to one request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Success,
    /// The user pressed Cancel (`LAError.userCancel`).
    Cancelled,
    Failed(Failure),
}

// LAError codes (LocalAuthentication's `LAError.h`).
const LA_AUTHENTICATION_FAILED: isize = -1;
const LA_USER_CANCEL: isize = -2;
const LA_SYSTEM_CANCEL: isize = -4;
const LA_PASSCODE_NOT_SET: isize = -5;
const LA_BIOMETRY_LOCKOUT: isize = -8;
const LA_APP_CANCEL: isize = -9;

impl Outcome {
    /// The outcome of LocalAuthentication's reply: whether it succeeded, and
    /// the `LAError` code, or `None` if there was no error or it wasn't
    /// LocalAuthentication's. Only success is success.
    pub fn from_reply(success: bool, code: Option<isize>) -> Outcome {
        if success {
            return Outcome::Success;
        }
        match code {
            Some(LA_USER_CANCEL) => Outcome::Cancelled,
            Some(LA_AUTHENTICATION_FAILED | LA_BIOMETRY_LOCKOUT) => {
                Outcome::Failed(Failure::NotAccepted)
            }
            Some(LA_SYSTEM_CANCEL | LA_APP_CANCEL) => Outcome::Failed(Failure::Interrupted),
            Some(LA_PASSCODE_NOT_SET) => Outcome::Failed(Failure::NoPassword),
            _ => Outcome::Failed(Failure::Unavailable),
        }
    }
}

/// Why [`Lock::unlock`] or [`Lock::confirm`] didn't go through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    /// The user cancelled, or Lax locked while asking. The lock screen
    /// stays as it is, with no message.
    Cancelled,
    /// The window is locked, so nothing was asked.
    Locked,
    /// Asking failed.
    Failed(Failure),
}

impl Refused {
    /// What to tell the user, if anything. A cancel says nothing.
    pub fn message(self) -> Option<&'static str> {
        match self {
            Refused::Cancelled => None,
            Refused::Locked => Some(LOCKED),
            Refused::Failed(failure) => Some(failure.message()),
        }
    }

    /// The command's error for the web UI ([`Refusal`]).
    pub fn refusal(self) -> Refusal {
        self.message().map(str::to_string)
    }
}

/// The error of a command that asks: a short message, or `None` when the
/// user cancelled, which the web UI shows as nothing.
pub type Refusal = Option<String>;

/// What asks the user. The real one is `macos::LocalAuthentication`; tests
/// use `fake::FakeAuthenticator`.
pub trait Authenticator: Send + Sync {
    /// One request's context. [`Lock`] makes one for each request and drops
    /// it when the answer is in, so a context never answers twice.
    type Context;

    /// A new context, never used before.
    fn new_context(&self) -> Self::Context;

    /// Asks the user with `context`, showing `reason`, and waits for the
    /// answer. Blocks, so never call it on the main thread.
    fn evaluate(&self, context: &Self::Context, reason: &str) -> Outcome;
}

/// Lax unlocked.
#[derive(Debug, Clone, Copy)]
struct Unlocked {
    /// When the user was last seen: the unlock, or the latest input since.
    last_seen: Stamp,
}

#[derive(Debug, Default)]
struct State {
    /// `None` while Lax is locked.
    unlocked: Option<Unlocked>,
    /// Whether the window is unlocked too. Never true while Lax is locked.
    window_unlocked: bool,
    /// Since when the window has been hidden (closed or minimised).
    window_hidden_since: Option<Stamp>,
    /// Changes when Lax locks, so the answer to a request made before it is
    /// thrown away.
    lax_generation: u64,
    /// Changes when the window locks, Lax's lock included. Kept apart from
    /// `lax_generation` so a window that stays hidden can't throw away the
    /// panel's unlock, which only needs Lax.
    window_generation: u64,
}

impl State {
    fn status(&self) -> Status {
        Status {
            unlocked: self.unlocked.is_some(),
            window_unlocked: self.unlocked.is_some() && self.window_unlocked,
        }
    }

    /// Locks Lax and the window.
    fn lock_lax(&mut self) -> Option<Locked> {
        self.lax_generation = self.lax_generation.wrapping_add(1);
        self.window_generation = self.window_generation.wrapping_add(1);
        self.window_unlocked = false;
        self.unlocked.take().map(|_| Locked::Lax)
    }

    /// Locks the window only.
    fn lock_window(&mut self) -> Option<Locked> {
        let was = self.status().window_unlocked;
        self.window_unlocked = false;
        if was {
            self.window_generation = self.window_generation.wrapping_add(1);
        }
        was.then_some(Locked::Window)
    }

    /// The generation a request from `asker` depends on.
    fn generation(&self, asker: Asker) -> u64 {
        match asker {
            Asker::Window => self.window_generation,
            Asker::Panel => self.lax_generation,
        }
    }

    fn unlock(&mut self, asker: Asker, now: Stamp) {
        self.unlocked = Some(Unlocked { last_seen: now });
        if asker == Asker::Window {
            self.window_unlocked = true;
            // The user is looking at it.
            self.window_hidden_since = None;
        }
    }

    /// The input rule. `idle` is how long since the last key press or mouse
    /// event, or `None` if it couldn't be read, which counts as no input.
    fn input(&mut self, now: Stamp, idle: Option<Duration>) -> Option<Locked> {
        let unlocked = self.unlocked.as_mut()?;
        // Checked before this reading too, so a gap with no readings, such
        // as a sleep the wall clock saw, counts even if the user has typed
        // since (for example their password at the Mac's lock screen).
        if unlocked.last_seen.passed(now, AWAY) {
            return self.lock_lax();
        }
        if let Some(input) = idle.and_then(|idle| now.before(idle))
            && input.mono > unlocked.last_seen.mono
        {
            unlocked.last_seen = input;
        }
        if unlocked.last_seen.passed(now, AWAY) {
            return self.lock_lax();
        }
        None
    }

    /// The hidden-window rule.
    fn window_visible(&mut self, now: Stamp, visible: bool) -> Option<Locked> {
        let locked = match self.window_hidden_since {
            Some(since) if since.passed(now, AWAY) => self.lock_window(),
            _ => None,
        };
        if visible {
            self.window_hidden_since = None;
        } else if self.window_hidden_since.is_none() {
            self.window_hidden_since = Some(now);
        }
        locked
    }
}

/// The app lock: its state, and the authenticator that asks.
pub struct Lock<A: Authenticator, C: Clock = SystemClock> {
    authenticator: A,
    clock: C,
    language: fn() -> Language,
    state: Mutex<State>,
    /// Held while a request asks, so only one dialog shows at a time.
    asking: Mutex<()>,
}

impl<A: Authenticator, C: Clock> Lock<A, C> {
    /// A lock that starts locked.
    pub fn new(authenticator: A, clock: C) -> Self {
        Lock {
            authenticator,
            clock,
            language: Language::system,
            state: Mutex::new(State::default()),
            asking: Mutex::new(()),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => {
                // Something panicked while holding the state: fail closed.
                let mut state = poisoned.into_inner();
                state.lock_lax();
                self.state.clear_poison();
                state
            }
        }
    }

    pub fn status(&self) -> Status {
        self.state().status()
    }

    /// The authenticator, for tests elsewhere that drive the fake.
    #[cfg(test)]
    pub fn authenticator(&self) -> &A {
        &self.authenticator
    }

    /// Asks the user to unlock, for `asker`. Only for something the user
    /// just did: showing the window or pressing Unlock. Asks nothing if it
    /// is already unlocked for `asker`.
    pub fn unlock(&self, asker: Asker) -> Result<(), Refused> {
        let generation = {
            let state = self.state();
            let status = state.status();
            let already = match asker {
                Asker::Window => status.window_unlocked,
                Asker::Panel => status.unlocked,
            };
            if already {
                return Ok(());
            }
            state.generation(asker)
        };
        let action = match asker {
            Asker::Window => Action::OpenWindow,
            Asker::Panel => Action::UseMemoryInDraft,
        };
        let outcome = self.ask(action);
        let mut state = self.state();
        if state.generation(asker) != generation {
            // Lax locked while asking.
            return Err(Refused::Cancelled);
        }
        match outcome {
            Outcome::Success => {
                state.unlock(asker, self.clock.now());
                Ok(())
            }
            Outcome::Cancelled => Err(Refused::Cancelled),
            Outcome::Failed(failure) => Err(Refused::Failed(failure)),
        }
    }

    /// Asks the user to confirm `action`, every time, in an unlocked window.
    /// Changes nothing about the lock. Refused without asking if the window
    /// is locked, and refused if it locks while asking.
    pub fn confirm(&self, action: Action) -> Result<(), Refused> {
        let generation = {
            let state = self.state();
            if !state.status().window_unlocked {
                return Err(Refused::Locked);
            }
            state.window_generation
        };
        let outcome = self.ask(action);
        let state = self.state();
        if state.window_generation != generation || !state.status().window_unlocked {
            return Err(Refused::Locked);
        }
        match outcome {
            Outcome::Success => Ok(()),
            Outcome::Cancelled => Err(Refused::Cancelled),
            Outcome::Failed(failure) => Err(Refused::Failed(failure)),
        }
    }

    /// Asks the user to confirm `action`, locked or not, for the few
    /// commands that may run before the lock screen ([`ASKS_WHILE_LOCKED`]).
    /// Changes nothing about the lock: it never unlocks the window. Refused
    /// without asking for any other action, and quietly if Lax locks while
    /// asking.
    pub fn confirm_while_locked(&self, action: Action) -> Result<(), Refused> {
        if !ASKS_WHILE_LOCKED
            .iter()
            .any(|(_, allowed)| *allowed == action)
        {
            return Err(Refused::Locked);
        }
        let generation = self.state().lax_generation;
        let outcome = self.ask(action);
        if self.state().lax_generation != generation {
            return Err(Refused::Cancelled);
        }
        match outcome {
            Outcome::Success => Ok(()),
            Outcome::Cancelled => Err(Refused::Cancelled),
            Outcome::Failed(failure) => Err(Refused::Failed(failure)),
        }
    }

    /// Runs `change` only once the user confirmed `action` ([`EVERY_TIME`]).
    /// A cancel, a failure, or the window locked before or while asking
    /// changes nothing.
    pub fn confirmed<T>(
        &self,
        action: Action,
        change: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, Refusal> {
        logged(self.confirm(action)).map_err(Refused::refusal)?;
        Ok(change()?)
    }

    /// [`Lock::confirmed`] for a command that may run while the window is
    /// locked ([`ASKS_WHILE_LOCKED`], [`Lock::confirm_while_locked`]).
    pub fn confirmed_while_locked<T>(
        &self,
        action: Action,
        change: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, Refusal> {
        logged(self.confirm_while_locked(action)).map_err(Refused::refusal)?;
        Ok(change()?)
    }

    /// A change that asks only when it loosens privacy ([`LOOSENING`]).
    /// `change(false)` compares its arguments with the current setting and
    /// either makes the change, or changes nothing and returns `None` when
    /// it would loosen privacy. Then the user is asked to confirm `action`,
    /// and only then `change(true)` makes the change. The compare and a
    /// change that doesn't ask happen under one hold of the store, so a
    /// change made meanwhile can't turn one into the other.
    pub fn confirmed_if_loosening<T>(
        &self,
        action: Action,
        mut change: impl FnMut(bool) -> Result<Option<T>, String>,
    ) -> Result<T, Refusal> {
        if let Some(done) = change(false)? {
            return Ok(done);
        }
        logged(self.confirm(action)).map_err(Refused::refusal)?;
        change(true)?.ok_or_else(|| Some("Nothing was changed.".to_string()))
    }

    /// One request, with a context of its own that is dropped when the
    /// answer is in. The state isn't held meanwhile, so a lock can come.
    /// While another request asks, this one is cancelled without asking: the
    /// user is already answering a dialog, and a second would only stack
    /// another on top.
    fn ask(&self, action: Action) -> Outcome {
        let _one_at_a_time = match self.asking.try_lock() {
            Ok(guard) => guard,
            // A request that panicked asks nothing more; the next may ask.
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => return Outcome::Cancelled,
        };
        let context = self.authenticator.new_context();
        let outcome = self
            .authenticator
            .evaluate(&context, action.reason((self.language)()));
        drop(context);
        outcome
    }

    /// The watcher's reading, every few seconds: how long since the last
    /// input (`None` if it couldn't be read) and whether the window is
    /// visible. Applies the input and hidden-window rules. Never asks.
    pub fn tick(&self, idle: Option<Duration>, window_visible: bool) -> Option<Locked> {
        let now = self.clock.now();
        let mut state = self.state();
        let window = state.window_visible(now, window_visible);
        state.input(now, idle).or(window)
    }

    /// The window was shown or hidden. Showing it after 5 minutes hidden
    /// locks it. Never asks.
    pub fn window_visible(&self, visible: bool) -> Option<Locked> {
        let now = self.clock.now();
        self.state().window_visible(now, visible)
    }

    /// The screen locked, the Mac slept or the user switched accounts: Lax
    /// locks at once. Never asks.
    pub fn mac_event(&self, _event: MacEvent) -> Option<Locked> {
        self.state().lock_lax()
    }

    /// The gate: whether `command`, called from the window labelled
    /// `window`, may run now. Default deny: a command without a rule, or
    /// from a window its rule doesn't name, is refused.
    pub fn allows(&self, command: &str, window: &str) -> bool {
        match (rule(command), window) {
            (Some(Rule::Open | Rule::AsksWhileLocked), MAIN_WINDOW) => true,
            (Some(Rule::Unlocked | Rule::EveryTime | Rule::Loosening), MAIN_WINDOW) => {
                self.status().window_unlocked
            }
            (Some(Rule::Panel), PANEL_WINDOW) => true,
            _ => false,
        }
    }
}

/// Logs how a confirmation ended: a flag, never the action, and never
/// anything captured.
fn logged(result: Result<(), Refused>) -> Result<(), Refused> {
    use crate::logging::{self, Field};
    match result {
        Ok(()) => logging::info("lock.confirmed", &[]),
        Err(Refused::Cancelled) => logging::info("lock.confirm_cancelled", &[]),
        Err(Refused::Locked) => logging::info("lock.confirm_locked", &[]),
        Err(Refused::Failed(failure)) => logging::info(
            "lock.confirm_failed",
            &[Field::Flag("no_password", failure == Failure::NoPassword)],
        ),
    }
    result
}

// The command lists (ADR 0012, "Enforced in Rust, failing closed"). Every
// `#[tauri::command]` in `lib.rs`'s handler is in exactly one of them
// (`lib::tests::every_command_has_a_lock_rule`).

/// What the gate answers a command it refuses. Fixed, and never says more.
pub const LOCKED: &str = "Lax is locked.";

/// The label of Lax's own window (`tauri.conf.json`).
pub const MAIN_WINDOW: &str = "main";
/// The label of the writing assistant's panel (`summon::PANEL`).
pub const PANEL_WINDOW: &str = "panel";

/// A command's lock rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rule {
    /// Allowed in the main window while it is locked: none of them returns
    /// anything Lax captured or learned. [`OPEN`].
    Open,
    /// Needs Lax and the window unlocked, and asks nothing more: the ADR's
    /// `none` list. [`UNLOCKED`].
    Unlocked,
    /// Needs the window unlocked, and the command asks again every time
    /// before it changes anything. [`EVERY_TIME`].
    EveryTime,
    /// Needs the window unlocked, and the command asks again when the change
    /// loosens privacy. [`LOOSENING`].
    Loosening,
    /// Allowed in the main window locked or not, and the command asks every
    /// time, without unlocking the window. [`ASKS_WHILE_LOCKED`].
    AsksWhileLocked,
    /// The writing assistant's own commands, from the panel only, locked or
    /// not. [`PANEL`].
    Panel,
}

/// The main window's commands that work while it is locked: the lock
/// itself, and opening the database, whose screens come before the lock
/// screen. Nothing else: the rest of the window, the setup guide and
/// onboarding included, shows only once it is unlocked (ADR 0012, question
/// 10).
pub const OPEN: &[&str] = &[
    "lock_status",
    "unlock",
    "database_status",
    "continue_opening",
    "retry_opening",
];

/// The main window's commands that need the window unlocked and ask
/// nothing more: reading, and ordinary settings (ADR 0012, "Doesn't ask,
/// once the window is unlocked").
pub const UNLOCKED: &[&str] = &[
    "accessibility_status",
    "open_accessibility_settings",
    "filevault_enabled",
    "capture_status",
    "setup_status",
    "bug_report_details",
    "recent_captures",
    "search_captures",
    "capture_detail",
    "captured_apps",
    "retention_days",
    "exclusion_settings",
    "add_exclusion",
    "find_local_models",
    "model_settings",
    "clear_model",
    "test_model",
    "outbox_entries",
    "outbox_payload",
    "outbox_retention_days",
    "journal_status",
    "journal_episodes",
    "ask_memory",
    "memory_status",
    "memory_entities",
    "recreate_page",
    "recreate_about_me",
    "page_history",
    "restore_page_version",
    "page_text",
    "save_page",
    "close_page",
    "pick_export_folder",
    "page_notes",
    "keep_entity",
    "rename_entity",
    "merge_entities",
    "obvious_pairs",
    "not_same",
    "merge_obvious_pairs",
    "set_part_of",
    "take_out_part",
    "merge_suggestions",
    "likely_same",
    "brief_on",
    "part_suggestions",
    "not_part",
    "dont_learn_list",
    "finish_setup",
    "writing_status",
    "set_writing_key",
];

/// The main window's commands that ask for Touch ID or the login password
/// every time, before they change anything, with the reason each one shows
/// (ADR 0012, "Asks every time"). They need the window unlocked too.
pub const EVERY_TIME: &[(&str, Action)] = &[
    ("forget_captures", Action::ForgetCaptures),
    ("delete_all_data", Action::DeleteAllData),
    ("forget_entity", Action::ForgetEntity),
    ("forget_memory", Action::ForgetMemory),
    ("delete_outbox", Action::DeleteOutbox),
    ("export_pages", Action::ExportPages),
    ("remove_exclusion", Action::RemoveExclusion),
    ("allow_learning", Action::AllowLearning),
    ("set_retention_days", Action::SetRetention),
    ("set_outbox_retention_days", Action::SetOutboxRetention),
    ("set_own_names", Action::SetOwnNames),
];

/// The main window's commands that ask only when the change loosens
/// privacy, by comparing their arguments with the current setting (ADR
/// 0012, "Asks when the change loosens privacy"). They need the window
/// unlocked too.
pub const LOOSENING: &[(&str, Action)] = &[
    ("set_default_excluded", Action::TurnOffDefaultExclusion),
    ("set_journal_enabled", Action::TurnOnJournal),
    ("set_memory_enabled", Action::TurnOnMemory),
    ("set_writing_enabled", Action::TurnOnWriting),
    ("set_model", Action::UseModelOffThisMac),
];

/// The main window's commands that ask every time and may run while it is
/// locked: Start over is on the database screens, which come before the
/// lock screen. One dialog, with Start over's reason, and the window stays
/// locked ([`Lock::confirm_while_locked`]).
pub const ASKS_WHILE_LOCKED: &[(&str, Action)] = &[("start_over", Action::StartOver)];

/// The action `command` asks the user to confirm, if it asks.
pub fn action(command: &str) -> Option<Action> {
    EVERY_TIME
        .iter()
        .chain(LOOSENING)
        .chain(ASKS_WHILE_LOCKED)
        .find(|(name, _)| *name == command)
        .map(|(_, action)| *action)
}

/// Whether setting a switch that is `current` to `wanted` turns it on: the
/// direction that loosens privacy for the journal, the memory and the
/// writing assistant. Turning one off, or on when it is on, doesn't ask.
pub fn turns_on(current: bool, wanted: bool) -> bool {
    wanted && !current
}

/// The panel's commands. They keep working while Lax is locked (ADR 0012,
/// "What stays unlocked"); `write_draft` will check the lock itself before
/// it uses the memory pages.
pub const PANEL: &[&str] = &[
    "panel_call",
    "write_draft",
    "fit_panel",
    "insert_draft",
    "close_panel",
];

/// The rule for `command`, or `None` if it has none, which the gate refuses.
pub fn rule(command: &str) -> Option<Rule> {
    let asks = |list: &[(&str, Action)]| list.iter().any(|(name, _)| *name == command);
    if asks(EVERY_TIME) {
        return Some(Rule::EveryTime);
    }
    if asks(LOOSENING) {
        return Some(Rule::Loosening);
    }
    if asks(ASKS_WHILE_LOCKED) {
        return Some(Rule::AsksWhileLocked);
    }
    [
        (OPEN, Rule::Open),
        (UNLOCKED, Rule::Unlocked),
        (PANEL, Rule::Panel),
    ]
    .into_iter()
    .find(|(list, _)| list.contains(&command))
    .map(|(_, rule)| rule)
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)] // LocalAuthentication calls; see SAFETY below.
pub mod macos {
    //! The real authenticator: LocalAuthentication's `LAContext`.

    use std::sync::mpsc;

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::Bool;
    use objc2_foundation::{NSError, NSLocale, NSString};
    use objc2_local_authentication::{LAContext, LAErrorDomain, LAPolicy};

    use super::{Authenticator, Failure, Outcome};

    /// Touch ID where the Mac has it, with "Use Password…", and the login
    /// password where it doesn't (`LAPolicyDeviceOwnerAuthentication`).
    pub struct LocalAuthentication;

    const POLICY: LAPolicy = LAPolicy::DeviceOwnerAuthentication;

    impl Authenticator for LocalAuthentication {
        type Context = Retained<LAContext>;

        fn new_context(&self) -> Retained<LAContext> {
            // SAFETY: makes a new, unused context; no preconditions.
            unsafe { LAContext::new() }
        }

        fn evaluate(&self, context: &Retained<LAContext>, reason: &str) -> Outcome {
            // An empty reason makes LocalAuthentication throw. The reasons
            // are fixed and never empty (`every_reason_is_localized`).
            if reason.is_empty() {
                return Outcome::Failed(Failure::Unavailable);
            }
            // SAFETY: a valid context and policy, and not called from a
            // reply block, where it could deadlock.
            if let Err(error) = unsafe { context.canEvaluatePolicy_error(POLICY) } {
                return Outcome::from_reply(false, code(&error));
            }
            let (sender, answer) = mpsc::sync_channel(1);
            let reply = RcBlock::new(move |success: Bool, error: *mut NSError| {
                // SAFETY: LocalAuthentication passes null or a valid error
                // that lives for the duration of the call.
                let code = unsafe { error.as_ref() }.and_then(code);
                // A second answer, or no one waiting, is dropped.
                let _ = sender.try_send(Outcome::from_reply(success.as_bool(), code));
            });
            let reason = NSString::from_str(reason);
            // SAFETY: the block is sendable, as the method requires: it only
            // sends on a channel, which may be done from any thread, and it
            // touches nothing else. `context` is borrowed until the answer is
            // in, so it stays alive while it asks.
            unsafe { context.evaluatePolicy_localizedReason_reply(POLICY, &reason, &reply) };
            drop(reply);
            // If LocalAuthentication drops the block without answering, the
            // sender goes with it, and that is a failure, never a success.
            answer
                .recv()
                .unwrap_or(Outcome::Failed(Failure::Unavailable))
        }
    }

    /// The `LAError` code of `error`, or `None` if it isn't
    /// LocalAuthentication's.
    fn code(error: &NSError) -> Option<isize> {
        // SAFETY: a constant string LocalAuthentication provides, never
        // changed.
        let domain = unsafe { LAErrorDomain };
        error.domain().isEqualToString(domain).then(|| error.code())
    }

    /// The user's first preferred language, as a BCP 47 tag.
    pub fn first_preferred_language() -> Option<String> {
        NSLocale::preferredLanguages()
            .firstObject()
            .map(|tag| tag.to_string())
    }
}

#[cfg(test)]
pub mod fake {
    //! An authenticator and a clock in memory, for tests. Never shows a
    //! dialog.

    use std::cell::Cell;
    use std::collections::VecDeque;
    use std::sync::Mutex;
    use std::time::{Duration, Instant, SystemTime};

    use super::{Authenticator, Clock, Failure, Outcome, Stamp};

    /// A context. Like an `LAContext`, once it has succeeded it succeeds
    /// again at once, without asking (the ADR's spike), so reusing one would
    /// show up as a missing request.
    pub struct FakeContext {
        pub id: usize,
        succeeded: Cell<bool>,
    }

    type Hook = Box<dyn FnOnce() + Send>;

    #[derive(Default)]
    pub struct FakeAuthenticator {
        /// The answers to the next requests, in order. With none left, a
        /// request fails.
        pub answers: Mutex<VecDeque<Outcome>>,
        /// How many contexts were made.
        pub contexts: Mutex<usize>,
        /// Each request the user saw: the context's id and the reason.
        pub requests: Mutex<Vec<(usize, String)>>,
        /// Runs while the next request is asking, before it answers.
        pub while_asking: Mutex<Option<Hook>>,
    }

    impl FakeAuthenticator {
        pub fn answering(answers: &[Outcome]) -> Self {
            let fake = FakeAuthenticator::default();
            fake.answers.lock().unwrap().extend(answers);
            fake
        }

        pub fn then(&self, answer: Outcome) {
            self.answers.lock().unwrap().push_back(answer);
        }

        pub fn requests(&self) -> usize {
            self.requests.lock().unwrap().len()
        }

        pub fn contexts(&self) -> usize {
            *self.contexts.lock().unwrap()
        }
    }

    impl Authenticator for FakeAuthenticator {
        type Context = FakeContext;

        fn new_context(&self) -> FakeContext {
            let mut contexts = self.contexts.lock().unwrap();
            *contexts += 1;
            FakeContext {
                id: *contexts,
                succeeded: Cell::new(false),
            }
        }

        fn evaluate(&self, context: &FakeContext, reason: &str) -> Outcome {
            if context.succeeded.get() {
                return Outcome::Success;
            }
            self.requests
                .lock()
                .unwrap()
                .push((context.id, reason.to_string()));
            let hook = self.while_asking.lock().unwrap().take();
            if let Some(hook) = hook {
                hook();
            }
            let outcome = self
                .answers
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Outcome::Failed(Failure::Unavailable));
            context.succeeded.set(outcome == Outcome::Success);
            outcome
        }
    }

    /// A clock that moves only when told.
    pub struct FakeClock(Mutex<Stamp>);

    impl Default for FakeClock {
        fn default() -> Self {
            FakeClock(Mutex::new(Stamp {
                mono: Instant::now(),
                wall: SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000),
            }))
        }
    }

    impl FakeClock {
        /// Time passes while the Mac is awake: both clocks move.
        pub fn advance(&self, by: Duration) {
            let mut now = self.0.lock().unwrap();
            now.mono += by;
            now.wall += by;
        }

        /// The Mac sleeps: only the wall clock moves.
        pub fn sleep(&self, by: Duration) {
            self.0.lock().unwrap().wall += by;
        }

        /// The wall clock is set back.
        pub fn set_back(&self, by: Duration) {
            self.0.lock().unwrap().wall -= by;
        }
    }

    impl Clock for FakeClock {
        fn now(&self) -> Stamp {
            *self.0.lock().unwrap()
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use super::fake::{FakeAuthenticator, FakeClock};
    use super::*;

    const SECOND: Duration = Duration::from_secs(1);
    const MINUTE: Duration = Duration::from_secs(60);
    /// How often the watcher reads, in these tests.
    const TICK: Duration = Duration::from_secs(5);

    type TestLock = Lock<FakeAuthenticator, FakeClock>;

    fn lock_answering(answers: &[Outcome]) -> TestLock {
        let mut lock = Lock::new(FakeAuthenticator::answering(answers), FakeClock::default());
        lock.language = || Language::English;
        lock
    }

    /// A lock with the window unlocked and visible, after one request.
    fn unlocked() -> TestLock {
        let lock = lock_answering(&[Outcome::Success]);
        lock.window_visible(true);
        lock.unlock(Asker::Window).unwrap();
        lock
    }

    fn open() -> Status {
        Status {
            unlocked: true,
            window_unlocked: true,
        }
    }

    fn locked() -> Status {
        Status {
            unlocked: false,
            window_unlocked: false,
        }
    }

    fn only_lax() -> Status {
        Status {
            unlocked: true,
            window_unlocked: false,
        }
    }

    /// Waits `span` in steps of [`TICK`], with the user typing every
    /// `input_every` (or not at all), and returns the first lock.
    fn wait(
        lock: &TestLock,
        span: Duration,
        input_every: Option<Duration>,
        visible: bool,
    ) -> Option<Locked> {
        let mut since_input = Duration::ZERO;
        let mut waited = Duration::ZERO;
        let mut first = None;
        while waited < span {
            lock.clock.advance(TICK);
            waited += TICK;
            since_input += TICK;
            if input_every.is_some_and(|every| since_input >= every) {
                since_input = Duration::ZERO;
            }
            first = first.or(lock.tick(Some(since_input), visible));
        }
        first
    }

    // Starting locked; only success unlocks.

    #[test]
    fn lax_starts_locked() {
        let lock = lock_answering(&[]);
        assert_eq!(lock.status(), locked());
        assert_eq!(lock.authenticator.requests(), 0);
    }

    #[test]
    fn only_success_unlocks() {
        let lock = lock_answering(&[Outcome::Success]);
        assert_eq!(lock.unlock(Asker::Window), Ok(()));
        assert_eq!(lock.status(), open());
    }

    #[test]
    fn cancel_failure_and_errors_leave_it_locked() {
        for outcome in [
            Outcome::Cancelled,
            Outcome::Failed(Failure::NotAccepted),
            Outcome::Failed(Failure::Interrupted),
            Outcome::Failed(Failure::NoPassword),
            Outcome::Failed(Failure::Unavailable),
        ] {
            for asker in [Asker::Window, Asker::Panel] {
                let lock = lock_answering(&[outcome]);
                assert!(lock.unlock(asker).is_err(), "{outcome:?}");
                assert_eq!(lock.status(), locked(), "{outcome:?}");
            }
        }
        // No answer at all fails too.
        let lock = lock_answering(&[]);
        assert_eq!(
            lock.unlock(Asker::Window),
            Err(Refused::Failed(Failure::Unavailable))
        );
        assert_eq!(lock.status(), locked());
    }

    #[test]
    fn a_cancel_says_nothing_and_a_failure_says_why() {
        let lock = lock_answering(&[Outcome::Cancelled]);
        let refused = lock.unlock(Asker::Window).unwrap_err();
        assert_eq!(refused, Refused::Cancelled);
        assert_eq!(refused.message(), None);

        for failure in [
            Failure::NotAccepted,
            Failure::Interrupted,
            Failure::NoPassword,
            Failure::Unavailable,
        ] {
            let lock = lock_answering(&[Outcome::Failed(failure)]);
            let refused = lock.unlock(Asker::Window).unwrap_err();
            assert_eq!(refused, Refused::Failed(failure));
            let message = refused.message().unwrap();
            assert!(!message.is_empty());
            assert!(message.ends_with('.'));
        }
    }

    #[test]
    fn local_authentication_replies_map_to_outcomes() {
        // Success is success, whatever error comes with it.
        assert_eq!(Outcome::from_reply(true, None), Outcome::Success);
        assert_eq!(Outcome::from_reply(true, Some(-2)), Outcome::Success);
        // LAError.userCancel: quiet.
        assert_eq!(Outcome::from_reply(false, Some(-2)), Outcome::Cancelled);
        // authenticationFailed, biometryLockout
        for code in [-1, -8] {
            assert_eq!(
                Outcome::from_reply(false, Some(code)),
                Outcome::Failed(Failure::NotAccepted)
            );
        }
        // systemCancel, appCancel
        for code in [-4, -9] {
            assert_eq!(
                Outcome::from_reply(false, Some(code)),
                Outcome::Failed(Failure::Interrupted)
            );
        }
        // passcodeNotSet
        assert_eq!(
            Outcome::from_reply(false, Some(-5)),
            Outcome::Failed(Failure::NoPassword)
        );
        // userFallback, biometryNotAvailable, invalidContext, notInteractive,
        // codes Lax has never seen, an error from another domain, no error.
        for code in [Some(-3), Some(-6), Some(-10), Some(-1004), Some(1), None] {
            assert_eq!(
                Outcome::from_reply(false, code),
                Outcome::Failed(Failure::Unavailable),
                "{code:?}"
            );
        }
    }

    // The window and the panel.

    #[test]
    fn the_panel_unlocks_lax_but_not_the_window() {
        let lock = lock_answering(&[Outcome::Success, Outcome::Success]);
        assert_eq!(lock.unlock(Asker::Panel), Ok(()));
        assert_eq!(lock.status(), only_lax());
        // The window still asks.
        assert_eq!(lock.unlock(Asker::Window), Ok(()));
        assert_eq!(lock.authenticator.requests(), 2);
        assert_eq!(lock.status(), open());
    }

    #[test]
    fn an_unlocked_asker_is_not_asked_again() {
        let lock = unlocked();
        assert_eq!(lock.unlock(Asker::Window), Ok(()));
        assert_eq!(lock.unlock(Asker::Panel), Ok(()));
        assert_eq!(lock.authenticator.requests(), 1);
    }

    // Away: input.

    #[test]
    fn five_minutes_without_input_locks() {
        let lock = unlocked();
        assert_eq!(wait(&lock, AWAY - TICK, None, true), None);
        assert_eq!(lock.status(), open());
        assert_eq!(wait(&lock, TICK, None, true), Some(Locked::Lax));
        assert_eq!(lock.status(), locked());
    }

    #[test]
    fn input_keeps_it_unlocked() {
        let lock = unlocked();
        assert_eq!(wait(&lock, 30 * MINUTE, Some(MINUTE), true), None);
        assert_eq!(lock.status(), open());
    }

    #[test]
    fn idle_time_that_cannot_be_read_counts_as_no_input() {
        let lock = unlocked();
        for _ in 0..(AWAY.as_secs() / TICK.as_secs() - 1) {
            lock.clock.advance(TICK);
            assert_eq!(lock.tick(None, true), None);
        }
        lock.clock.advance(TICK);
        assert_eq!(lock.tick(None, true), Some(Locked::Lax));
    }

    #[test]
    fn the_input_rule_locks_the_panel_too() {
        let lock = lock_answering(&[Outcome::Success]);
        lock.unlock(Asker::Panel).unwrap();
        assert_eq!(wait(&lock, AWAY, None, false), Some(Locked::Lax));
        assert_eq!(lock.status(), locked());
    }

    #[test]
    fn sleep_counts_by_the_wall_clock() {
        let lock = unlocked();
        lock.clock.advance(MINUTE);
        assert_eq!(lock.tick(Some(SECOND), true), None);
        // The monotonic clock stands still while the Mac sleeps, and typing
        // the password at the lock screen may count as input.
        lock.clock.sleep(10 * MINUTE);
        assert_eq!(lock.tick(Some(SECOND), true), Some(Locked::Lax));
        assert_eq!(lock.status(), locked());
    }

    #[test]
    fn a_short_sleep_does_not_lock_by_itself() {
        let lock = unlocked();
        lock.clock.advance(MINUTE);
        assert_eq!(lock.tick(Some(SECOND), true), None);
        lock.clock.sleep(MINUTE);
        assert_eq!(lock.tick(Some(SECOND), true), None);
        assert_eq!(lock.status(), open());
    }

    #[test]
    fn a_clock_set_back_does_not_extend_the_time() {
        let lock = unlocked();
        lock.clock.set_back(60 * MINUTE);
        assert_eq!(wait(&lock, AWAY - TICK, None, true), None);
        assert_eq!(wait(&lock, TICK, None, true), Some(Locked::Lax));
    }

    #[test]
    fn the_screen_lock_sleep_and_account_switch_lock_at_once() {
        for event in [
            MacEvent::ScreenLocked,
            MacEvent::Slept,
            MacEvent::SwitchedAccount,
        ] {
            let lock = unlocked();
            assert_eq!(lock.mac_event(event), Some(Locked::Lax));
            assert_eq!(lock.status(), locked());
            // Already locked: nothing more locks.
            assert_eq!(lock.mac_event(event), None);

            let lock = lock_answering(&[Outcome::Success]);
            lock.unlock(Asker::Panel).unwrap();
            assert_eq!(lock.mac_event(event), Some(Locked::Lax));
            assert_eq!(lock.status(), locked());
        }
    }

    // Away: the hidden window.

    #[test]
    fn a_window_hidden_for_five_minutes_locks_but_the_panel_stays_unlocked() {
        let lock = unlocked();
        lock.window_visible(false);
        // The user keeps working in other apps.
        assert_eq!(wait(&lock, AWAY - TICK, Some(10 * SECOND), false), None);
        assert_eq!(lock.status(), open());
        assert_eq!(
            wait(&lock, TICK, Some(10 * SECOND), false),
            Some(Locked::Window)
        );
        assert_eq!(lock.status(), only_lax());
        // Showing it again doesn't unlock it, and asks nothing by itself.
        assert_eq!(lock.window_visible(true), None);
        assert_eq!(lock.status(), only_lax());
        assert_eq!(lock.authenticator.requests(), 1);
    }

    #[test]
    fn showing_the_window_after_five_minutes_hidden_locks_it() {
        // Without a watcher reading in between.
        let lock = unlocked();
        lock.window_visible(false);
        lock.clock.advance(AWAY);
        assert_eq!(lock.window_visible(true), Some(Locked::Window));
        assert_eq!(lock.status(), only_lax());
    }

    #[test]
    fn a_window_hidden_briefly_stays_unlocked() {
        let lock = unlocked();
        lock.window_visible(false);
        lock.clock.advance(AWAY - SECOND);
        assert_eq!(lock.window_visible(true), None);
        // Hidden again: the time starts over.
        lock.window_visible(false);
        lock.clock.advance(AWAY - SECOND);
        assert_eq!(lock.window_visible(true), None);
        assert_eq!(lock.status(), open());
    }

    #[test]
    fn the_hidden_window_counts_sleep_and_ignores_a_clock_set_back() {
        let lock = unlocked();
        lock.window_visible(false);
        lock.clock.sleep(AWAY);
        assert_eq!(lock.window_visible(true), Some(Locked::Window));

        let lock = unlocked();
        lock.window_visible(false);
        lock.clock.set_back(60 * MINUTE);
        lock.clock.advance(AWAY);
        assert_eq!(lock.window_visible(true), Some(Locked::Window));
    }

    #[test]
    fn a_window_locked_while_hidden_unlocks_again() {
        let lock = lock_answering(&[Outcome::Success, Outcome::Success]);
        lock.unlock(Asker::Panel).unwrap();
        lock.window_visible(false);
        lock.clock.advance(AWAY);
        assert_eq!(lock.window_visible(true), None); // Never unlocked.
        lock.unlock(Asker::Window).unwrap();
        assert_eq!(lock.status(), open());
        assert_eq!(lock.tick(Some(SECOND), true), None);
        assert_eq!(lock.status(), open());
    }

    // Contexts.

    #[test]
    fn each_request_uses_a_new_context() {
        let lock = lock_answering(&[
            Outcome::Cancelled,
            Outcome::Failed(Failure::NotAccepted),
            Outcome::Success,
            Outcome::Success,
            Outcome::Success,
        ]);
        let _ = lock.unlock(Asker::Window);
        let _ = lock.unlock(Asker::Window);
        lock.unlock(Asker::Window).unwrap();
        lock.confirm(Action::ForgetCaptures).unwrap();
        lock.confirm(Action::ForgetCaptures).unwrap();
        let requests = lock.authenticator.requests.lock().unwrap().clone();
        let ids: Vec<usize> = requests.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, [1, 2, 3, 4, 5]);
        assert_eq!(lock.authenticator.contexts(), 5);
    }

    #[test]
    fn a_lock_drops_the_context() {
        type Way = fn(&TestLock);
        let ways: [(&str, Way); 5] = [
            ("idle", |lock| {
                wait(lock, AWAY, None, true);
            }),
            ("screen lock", |lock| {
                lock.mac_event(MacEvent::ScreenLocked);
            }),
            ("sleep", |lock| {
                lock.mac_event(MacEvent::Slept);
            }),
            ("account switch", |lock| {
                lock.mac_event(MacEvent::SwitchedAccount);
            }),
            ("window hidden", |lock| {
                lock.window_visible(false);
                wait(lock, AWAY, Some(10 * SECOND), false);
                lock.window_visible(true);
            }),
        ];
        for (name, lock_it) in ways {
            let lock = unlocked();
            lock_it(&lock);
            assert!(!lock.status().window_unlocked, "{name}");
            // The next request makes a new context and asks.
            lock.authenticator.then(Outcome::Success);
            assert_eq!(lock.unlock(Asker::Window), Ok(()), "{name}");
            assert_eq!(lock.authenticator.contexts(), 2, "{name}");
            assert_eq!(lock.authenticator.requests(), 2, "{name}");
            assert_eq!(lock.status(), open(), "{name}");
        }
    }

    #[test]
    fn a_lock_while_asking_throws_the_answer_away() {
        let lock = Arc::new(lock_answering(&[Outcome::Success]));
        let during = Arc::clone(&lock);
        *lock.authenticator.while_asking.lock().unwrap() = Some(Box::new(move || {
            during.mac_event(MacEvent::Slept);
        }));
        assert_eq!(lock.unlock(Asker::Window), Err(Refused::Cancelled));
        assert_eq!(lock.status(), locked());
    }

    #[test]
    fn a_hidden_window_never_throws_away_the_panels_unlock() {
        // The window has been closed for longer than the away time, and the
        // watcher keeps ticking while the panel asks.
        let lock = Arc::new(lock_answering(&[Outcome::Success]));
        lock.window_visible(false);
        lock.clock.advance(AWAY + MINUTE);
        assert_eq!(lock.tick(Some(SECOND), false), None);
        let during = Arc::clone(&lock);
        *lock.authenticator.while_asking.lock().unwrap() = Some(Box::new(move || {
            during.clock.advance(SECOND);
            during.tick(Some(SECOND), false);
            during.clock.advance(SECOND);
            during.tick(Some(SECOND), false);
        }));
        assert_eq!(lock.unlock(Asker::Panel), Ok(()));
        assert_eq!(lock.status(), only_lax());
    }

    #[test]
    fn a_window_lock_while_confirming_refuses() {
        let lock = Arc::new(unlocked());
        lock.authenticator.then(Outcome::Success);
        lock.window_visible(false);
        lock.clock.advance(AWAY - SECOND);
        assert_eq!(lock.tick(Some(SECOND), false), None);
        let during = Arc::clone(&lock);
        *lock.authenticator.while_asking.lock().unwrap() = Some(Box::new(move || {
            during.clock.advance(SECOND);
            during.tick(Some(SECOND), false);
        }));
        assert_eq!(lock.confirm(Action::DeleteAllData), Err(Refused::Locked));
        assert_eq!(lock.status(), only_lax());
    }

    // Asking only when the user did something.

    #[test]
    fn nothing_asks_without_a_user_action() {
        let lock = unlocked();
        let asked = lock.authenticator.requests();
        // Locking while the window is visible shows the lock screen and asks
        // nothing.
        wait(&lock, AWAY, None, true);
        assert_eq!(lock.status(), locked());
        lock.window_visible(false);
        lock.window_visible(true);
        wait(&lock, 30 * MINUTE, Some(MINUTE), true);
        for event in [
            MacEvent::ScreenLocked,
            MacEvent::Slept,
            MacEvent::SwitchedAccount,
        ] {
            lock.mac_event(event);
        }
        lock.status();
        assert_eq!(lock.authenticator.requests(), asked);
        assert_eq!(lock.authenticator.contexts(), asked);
        assert_eq!(lock.status(), locked());
    }

    // Confirming, every time.

    #[test]
    fn confirm_asks_every_time_also_twice_in_a_row() {
        let lock = unlocked();
        for _ in 0..2 {
            lock.authenticator.then(Outcome::Success);
            assert_eq!(lock.confirm(Action::DeleteAllData), Ok(()));
        }
        assert_eq!(lock.authenticator.requests(), 3);
        let requests = lock.authenticator.requests.lock().unwrap().clone();
        assert_eq!(
            requests[1].1,
            Action::DeleteAllData.reason(Language::English)
        );
        assert_eq!(lock.status(), open());
    }

    #[test]
    fn confirm_refuses_without_asking_while_the_window_is_locked() {
        let lock = lock_answering(&[Outcome::Success, Outcome::Success]);
        assert_eq!(lock.confirm(Action::ExportPages), Err(Refused::Locked));
        lock.unlock(Asker::Panel).unwrap();
        assert_eq!(lock.confirm(Action::ExportPages), Err(Refused::Locked));
        assert_eq!(lock.authenticator.requests(), 1);
    }

    #[test]
    fn a_refused_confirm_leaves_the_lock_as_it_was() {
        for outcome in [Outcome::Cancelled, Outcome::Failed(Failure::NotAccepted)] {
            let lock = unlocked();
            lock.authenticator.then(outcome);
            assert!(lock.confirm(Action::RemoveExclusion).is_err());
            assert_eq!(lock.status(), open());
        }
    }

    // Asking again: the commands' side (ADR 0012, "What else asks").

    const REFUSALS: [Outcome; 5] = [
        Outcome::Cancelled,
        Outcome::Failed(Failure::NotAccepted),
        Outcome::Failed(Failure::Interrupted),
        Outcome::Failed(Failure::NoPassword),
        Outcome::Failed(Failure::Unavailable),
    ];

    /// What a refused confirmation answers the web UI: nothing for a
    /// cancel, a short message otherwise.
    fn refusal(outcome: Outcome) -> Refusal {
        match outcome {
            Outcome::Cancelled => None,
            Outcome::Failed(failure) => Some(failure.message().to_string()),
            Outcome::Success => unreachable!(),
        }
    }

    #[test]
    fn every_time_asks_on_every_call_and_changes_only_on_success() {
        for (command, action) in EVERY_TIME {
            let lock = unlocked();
            let changes = Cell::new(0);
            let change = || {
                changes.set(changes.get() + 1);
                Ok(changes.get())
            };
            for outcome in REFUSALS {
                lock.authenticator.then(outcome);
                assert_eq!(
                    lock.confirmed(*action, change),
                    Err(refusal(outcome)),
                    "{command}: {outcome:?}"
                );
                assert_eq!(changes.get(), 0, "{command}: {outcome:?} changed something");
            }
            // Twice in a row: asked twice.
            for time in 1..=2 {
                lock.authenticator.then(Outcome::Success);
                assert_eq!(lock.confirmed(*action, change), Ok(time), "{command}");
            }
            let requests = lock.authenticator.requests.lock().unwrap().clone();
            // The unlock, the refusals and the two successes.
            assert_eq!(requests.len(), 1 + REFUSALS.len() + 2, "{command}");
            for (_, reason) in &requests[1..] {
                assert_eq!(reason, action.reason(Language::English), "{command}");
            }
            // Confirming never changes the lock.
            assert_eq!(lock.status(), open(), "{command}");
        }
    }

    #[test]
    fn a_change_that_fails_after_asking_says_why() {
        let lock = unlocked();
        lock.authenticator.then(Outcome::Success);
        let failed: Result<(), Refusal> = lock.confirmed(Action::ForgetCaptures, || {
            Err("Lax couldn't change it.".into())
        });
        assert_eq!(failed, Err(Some("Lax couldn't change it.".to_string())));
    }

    #[test]
    fn asking_again_needs_the_window_unlocked() {
        let changed = Cell::new(false);
        let change = || {
            changed.set(true);
            Ok(())
        };
        let locked_out = Err(Some(LOCKED.to_string()));

        // Locked, or only Lax unlocked from the panel: refused without asking.
        let lock = lock_answering(&[Outcome::Success]);
        assert_eq!(lock.confirmed(Action::DeleteAllData, change), locked_out);
        lock.unlock(Asker::Panel).unwrap();
        assert_eq!(lock.confirmed(Action::DeleteAllData, change), locked_out);
        assert_eq!(
            lock.confirmed_if_loosening(Action::TurnOnJournal, |_| Ok(None::<()>)),
            locked_out
        );
        assert_eq!(lock.authenticator.requests(), 1);

        // Locked while asking: the answer is thrown away.
        for action in [Action::ForgetEntity, Action::TurnOnMemory] {
            let lock = Arc::new(unlocked());
            lock.authenticator.then(Outcome::Success);
            let during = Arc::clone(&lock);
            *lock.authenticator.while_asking.lock().unwrap() = Some(Box::new(move || {
                during.mac_event(MacEvent::ScreenLocked);
            }));
            let result = if action == Action::ForgetEntity {
                lock.confirmed(action, change)
            } else {
                lock.confirmed_if_loosening(action, |confirmed| {
                    if confirmed {
                        changed.set(true);
                    }
                    Ok(confirmed.then_some(()))
                })
            };
            assert_eq!(result, locked_out, "{action:?}");
            assert_eq!(lock.status(), locked(), "{action:?}");
        }
        assert!(!changed.get());
    }

    /// A switch, as the journal's, the memory's and the writing assistant's
    /// setting, behind [`Lock::confirmed_if_loosening`] as their commands
    /// use it.
    fn set_switch(lock: &TestLock, switch: &Cell<bool>, wanted: bool) -> Result<bool, Refusal> {
        lock.confirmed_if_loosening(Action::TurnOnJournal, |confirmed| {
            if !confirmed && turns_on(switch.get(), wanted) {
                return Ok(None);
            }
            switch.set(wanted);
            Ok(Some(wanted))
        })
    }

    #[test]
    fn loosening_asks_only_in_the_loosening_direction() {
        let lock = unlocked();
        let switch = Cell::new(false);
        // Off to off, and on to off: no asking.
        assert_eq!(set_switch(&lock, &switch, false), Ok(false));
        assert_eq!(lock.authenticator.requests(), 1);

        // Off to on: asks, and a refusal leaves it off.
        for outcome in REFUSALS {
            lock.authenticator.then(outcome);
            assert_eq!(set_switch(&lock, &switch, true), Err(refusal(outcome)));
            assert!(!switch.get(), "{outcome:?}");
        }
        lock.authenticator.then(Outcome::Success);
        assert_eq!(set_switch(&lock, &switch, true), Ok(true));
        assert!(switch.get());
        let asked = lock.authenticator.requests();
        assert_eq!(asked, 1 + REFUSALS.len() + 1);
        let requests = lock.authenticator.requests.lock().unwrap().clone();
        assert_eq!(
            requests.last().unwrap().1,
            Action::TurnOnJournal.reason(Language::English)
        );

        // On to on, and on to off: no asking.
        assert_eq!(set_switch(&lock, &switch, true), Ok(true));
        assert_eq!(set_switch(&lock, &switch, false), Ok(false));
        assert!(!switch.get());
        assert_eq!(lock.authenticator.requests(), asked);
    }

    #[test]
    fn only_turning_a_switch_on_loosens() {
        assert!(turns_on(false, true));
        assert!(!turns_on(true, true));
        assert!(!turns_on(false, false));
        assert!(!turns_on(true, false));
    }

    #[test]
    fn start_over_asks_once_while_locked_and_leaves_it_locked() {
        let lock = lock_answering(&[Outcome::Success]);
        let changes = Cell::new(0);
        let change = || {
            changes.set(changes.get() + 1);
            Ok(())
        };
        assert_eq!(
            lock.confirmed_while_locked(Action::StartOver, change),
            Ok(())
        );
        assert_eq!(changes.get(), 1);
        // One dialog, with Start over's reason, and nothing unlocked.
        let requests = lock.authenticator.requests.lock().unwrap().clone();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].1, Action::StartOver.reason(Language::English));
        assert_eq!(lock.status(), locked());

        // A refusal changes nothing.
        for outcome in REFUSALS {
            lock.authenticator.then(outcome);
            assert_eq!(
                lock.confirmed_while_locked(Action::StartOver, change),
                Err(refusal(outcome))
            );
        }
        assert_eq!(changes.get(), 1);

        // Unlocked, it asks too, and the window stays as it was.
        let lock = unlocked();
        lock.authenticator.then(Outcome::Success);
        assert_eq!(
            lock.confirmed_while_locked(Action::StartOver, change),
            Ok(())
        );
        assert_eq!(lock.authenticator.requests(), 2);
        assert_eq!(lock.status(), open());
    }

    #[test]
    fn only_start_over_asks_while_locked() {
        for action in Action::ALL {
            if ASKS_WHILE_LOCKED.iter().any(|(_, a)| *a == action) {
                continue;
            }
            let lock = lock_answering(&[Outcome::Success]);
            assert_eq!(
                lock.confirm_while_locked(action),
                Err(Refused::Locked),
                "{action:?}"
            );
            assert_eq!(lock.authenticator.requests(), 0, "{action:?}");
        }
    }

    #[test]
    fn a_lock_while_starting_over_is_a_quiet_cancel() {
        let lock = Arc::new(lock_answering(&[Outcome::Success, Outcome::Success]));
        lock.unlock(Asker::Panel).unwrap();
        let during = Arc::clone(&lock);
        *lock.authenticator.while_asking.lock().unwrap() = Some(Box::new(move || {
            during.mac_event(MacEvent::Slept);
        }));
        assert_eq!(
            lock.confirm_while_locked(Action::StartOver),
            Err(Refused::Cancelled)
        );
    }

    #[test]
    fn each_command_that_asks_has_one_action_and_its_rule() {
        let lists: [(&[(&str, Action)], Rule); 3] = [
            (EVERY_TIME, Rule::EveryTime),
            (LOOSENING, Rule::Loosening),
            (ASKS_WHILE_LOCKED, Rule::AsksWhileLocked),
        ];
        let mut actions = Vec::new();
        for (list, wanted) in lists {
            for (command, act) in list {
                assert_eq!(rule(command), Some(wanted), "{command}");
                assert_eq!(action(command), Some(*act), "{command}");
                assert!(!actions.contains(act), "{act:?} is used twice");
                actions.push(*act);
                // Asking is for what the user does in the window.
                assert!(!OPEN.contains(command) && !UNLOCKED.contains(command));
                assert!(!PANEL.contains(command), "{command}");
            }
        }
        // Unlocking isn't a command's confirmation.
        for act in [Action::OpenWindow, Action::UseMemoryInDraft] {
            assert!(!actions.contains(&act), "{act:?}");
        }
        for command in OPEN.iter().chain(UNLOCKED).chain(PANEL) {
            assert_eq!(action(command), None, "{command}");
        }
    }

    #[test]
    fn commands_that_ask_need_the_unlocked_window_but_start_over() {
        let lock = lock_answering(&[Outcome::Success, Outcome::Success]);
        let asking = || {
            EVERY_TIME
                .iter()
                .chain(LOOSENING)
                .map(|(command, _)| *command)
        };
        for command in asking() {
            assert!(!lock.allows(command, MAIN), "{command} while locked");
        }
        lock.unlock(Asker::Panel).unwrap();
        for command in asking() {
            assert!(!lock.allows(command, MAIN), "{command} with only Lax");
        }
        lock.unlock(Asker::Window).unwrap();
        for command in asking() {
            assert!(lock.allows(command, MAIN), "{command} unlocked");
            assert!(
                !lock.allows(command, PANEL_LABEL),
                "{command} from the panel"
            );
        }
        // Start over: from the main window, locked or not, never the panel.
        let lock = lock_answering(&[Outcome::Success]);
        for (command, _) in ASKS_WHILE_LOCKED {
            assert!(lock.allows(command, MAIN), "{command} while locked");
            assert!(
                !lock.allows(command, PANEL_LABEL),
                "{command} from the panel"
            );
        }
        lock.unlock(Asker::Window).unwrap();
        for (command, _) in ASKS_WHILE_LOCKED {
            assert!(lock.allows(command, MAIN), "{command} unlocked");
        }
    }

    #[test]
    fn one_request_asks_at_a_time() {
        let lock = Arc::new(lock_answering(&[Outcome::Success]));
        let during = Arc::clone(&lock);
        let second = Arc::new(Mutex::new(None));
        let seen = Arc::clone(&second);
        *lock.authenticator.while_asking.lock().unwrap() = Some(Box::new(move || {
            *seen.lock().unwrap() = Some(during.unlock(Asker::Window));
        }));
        assert_eq!(lock.unlock(Asker::Window), Ok(()));
        // The second was cancelled quietly, without a dialog of its own.
        assert_eq!(*second.lock().unwrap(), Some(Err(Refused::Cancelled)));
        assert_eq!(lock.authenticator.requests(), 1);
        assert_eq!(lock.authenticator.contexts(), 1);
        assert_eq!(lock.status(), open());
        // Once answered, the next request asks.
        lock.authenticator.then(Outcome::Success);
        assert_eq!(lock.confirm(Action::ForgetMemory), Ok(()));
        assert_eq!(lock.authenticator.requests(), 2);
    }

    // The gate.

    const MAIN: &str = MAIN_WINDOW;
    const PANEL_LABEL: &str = PANEL_WINDOW;

    #[test]
    fn locked_commands_are_refused() {
        let lock = lock_answering(&[Outcome::Success, Outcome::Success]);
        for command in UNLOCKED {
            assert!(!lock.allows(command, MAIN), "{command} while locked");
        }
        // Unlocked from the panel: Lax, but not the window.
        lock.unlock(Asker::Panel).unwrap();
        for command in UNLOCKED {
            assert!(!lock.allows(command, MAIN), "{command} with only Lax");
        }
        lock.unlock(Asker::Window).unwrap();
        for command in UNLOCKED {
            assert!(lock.allows(command, MAIN), "{command} unlocked");
            // Never from the panel, whatever the lock.
            assert!(
                !lock.allows(command, PANEL_LABEL),
                "{command} from the panel"
            );
        }
        // Locked again: refused again.
        lock.mac_event(MacEvent::ScreenLocked);
        for command in UNLOCKED {
            assert!(!lock.allows(command, MAIN), "{command} locked again");
        }
    }

    #[test]
    fn open_commands_work_while_locked_from_the_main_window_only() {
        let lock = lock_answering(&[Outcome::Success]);
        for command in OPEN {
            assert!(lock.allows(command, MAIN), "{command}");
            assert!(
                !lock.allows(command, PANEL_LABEL),
                "{command} from the panel"
            );
        }
        lock.unlock(Asker::Window).unwrap();
        for command in OPEN {
            assert!(lock.allows(command, MAIN), "{command}");
            assert!(
                !lock.allows(command, PANEL_LABEL),
                "{command} from the panel"
            );
        }
        // Nothing open returns what Lax captured or learned.
        for command in [
            "recent_captures",
            "journal_episodes",
            "ask_memory",
            "page_text",
        ] {
            assert_ne!(rule(command), Some(Rule::Open), "{command}");
        }
    }

    #[test]
    fn panel_commands_work_from_the_panel_only_locked_or_not() {
        let lock = lock_answering(&[Outcome::Success]);
        for command in PANEL {
            assert!(lock.allows(command, PANEL_LABEL), "{command} while locked");
            assert!(
                !lock.allows(command, MAIN),
                "{command} from the main window"
            );
        }
        lock.unlock(Asker::Window).unwrap();
        for command in PANEL {
            assert!(lock.allows(command, PANEL_LABEL), "{command} unlocked");
            assert!(
                !lock.allows(command, MAIN),
                "{command} from the main window"
            );
        }
    }

    #[test]
    fn a_command_without_a_rule_or_from_another_window_is_refused() {
        let lock = unlocked();
        for command in ["", "new_command", "Recent_Captures", "recent_captures "] {
            assert_eq!(rule(command), None, "{command:?}");
            assert!(!lock.allows(command, MAIN), "{command:?}");
            assert!(!lock.allows(command, PANEL_LABEL), "{command:?}");
        }
        for window in ["", "Main", "settings", "panel2"] {
            assert!(!lock.allows("recent_captures", window), "{window:?}");
            assert!(!lock.allows("lock_status", window), "{window:?}");
            assert!(!lock.allows("write_draft", window), "{window:?}");
        }
    }

    #[test]
    fn the_window_labels_are_the_apps() {
        assert_eq!(PANEL_WINDOW, crate::summon::PANEL);
        let config = include_str!("../tauri.conf.json");
        assert!(config.contains(&format!("\"label\": \"{MAIN_WINDOW}\"")));
    }

    // Failing closed.

    #[test]
    fn a_poisoned_state_fails_closed() {
        let lock = Arc::new(unlocked());
        let poison = Arc::clone(&lock);
        let _ = std::thread::spawn(move || {
            let _state = poison.state.lock().unwrap();
            panic!("poison the state");
        })
        .join();
        assert!(lock.state.is_poisoned());
        assert_eq!(lock.status(), locked());
        assert!(!lock.state.is_poisoned());
    }

    // The reasons.

    #[test]
    fn every_reason_is_localized() {
        for language in [Language::English, Language::Swedish] {
            let mut seen = std::collections::HashSet::new();
            for action in Action::ALL {
                let reason = action.reason(language);
                let first = reason.chars().next();
                assert!(
                    first.is_some_and(char::is_lowercase),
                    "{action:?} in {language:?}: {reason:?}"
                );
                assert!(!reason.ends_with('.'), "{action:?} in {language:?}");
                assert_eq!(reason.trim(), reason, "{action:?} in {language:?}");
                assert!(seen.insert(reason), "{action:?} repeats {reason:?}");
            }
        }
        for action in Action::ALL {
            assert_ne!(
                action.reason(Language::English),
                action.reason(Language::Swedish),
                "{action:?} isn't translated"
            );
        }
    }

    #[test]
    fn the_system_language_picks_the_reasons() {
        for tag in ["sv", "sv-SE", "sv-FI", "SV_se"] {
            assert_eq!(Language::from_tag(tag), Language::Swedish, "{tag}");
        }
        for tag in ["en", "en-GB", "en-US", "nb-NO", "fi-FI", "de", "svx", ""] {
            assert_eq!(Language::from_tag(tag), Language::English, "{tag}");
        }
        // Whatever this Mac is set to, it reads as one of the two.
        let _ = Language::system();
    }

    #[test]
    fn the_reason_follows_the_language() {
        let mut lock = lock_answering(&[Outcome::Success]);
        lock.language = || Language::Swedish;
        lock.unlock(Asker::Window).unwrap();
        let requests = lock.authenticator.requests.lock().unwrap().clone();
        assert_eq!(requests[0].1, "öppna ditt minne");
    }

    /// The real LocalAuthentication. Ignored by default: it shows the
    /// Touch ID or password dialog twice. Run on a Mac with:
    ///
    /// `cargo test --locked lock::tests::live_unlock -- --ignored`
    ///
    /// and approve both dialogs. The second comes after a lock, so it shows
    /// that a new context asks again instead of answering at once.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "shows the Touch ID or password dialog"]
    fn live_unlock() {
        let lock = Lock::new(macos::LocalAuthentication, SystemClock);
        assert_eq!(lock.unlock(Asker::Window), Ok(()));
        assert_eq!(lock.status(), open());
        lock.mac_event(MacEvent::ScreenLocked);
        assert_eq!(lock.status(), locked());
        let asked = std::time::Instant::now();
        assert_eq!(lock.unlock(Asker::Window), Ok(()));
        // A context that answered at once would take no time at all.
        assert!(asked.elapsed() > Duration::from_millis(300));
        assert_eq!(lock.status(), open());
    }

    /// The real LocalAuthentication, cancelled. Ignored by default: it
    /// shows the dialog. Run with
    ///
    /// `cargo test --locked lock::tests::live_cancel -- --ignored`
    ///
    /// and press Cancel.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "shows the Touch ID or password dialog"]
    fn live_cancel() {
        let lock = Lock::new(macos::LocalAuthentication, SystemClock);
        assert_eq!(lock.unlock(Asker::Window), Err(Refused::Cancelled));
        assert_eq!(lock.status(), locked());
    }
}
