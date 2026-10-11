//! Pausing capture (GUARDRAILS §4).
//!
//! While paused, Lax reads nothing at all, not even app names. Everything that
//! reads from the screen must check [`Pause::is_paused`] immediately before
//! reading, and skip the read if it returns `true`. Work already under way,
//! such as a model call, runs through [`Pause::unless_paused`], so a pause
//! stops it too.
//!
//! Timed pauses end on the wall clock, so a pause still ends on time if the Mac
//! sleeps in between.

use std::future::{Future, poll_fn};
use std::pin::pin;
use std::sync::{Mutex, PoisonError};
use std::task::Poll;
use std::time::Duration;

use chrono::{DateTime, Local, NaiveTime, TimeDelta, TimeZone, Utc};

/// The pause options offered in the menu bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PauseLength {
    FifteenMinutes,
    OneHour,
    /// Until midnight, local time.
    UntilTomorrow,
    /// Until the user resumes capture.
    UntilResumed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CaptureState {
    #[default]
    Running,
    /// `until: None` means paused until the user resumes.
    Paused { until: Option<DateTime<Utc>> },
}

impl CaptureState {
    pub fn paused<Tz: TimeZone>(length: PauseLength, now: &DateTime<Tz>) -> Self {
        let until = match length {
            PauseLength::FifteenMinutes => Some(now.to_utc() + TimeDelta::minutes(15)),
            PauseLength::OneHour => Some(now.to_utc() + TimeDelta::hours(1)),
            PauseLength::UntilTomorrow => Some(next_midnight(now).to_utc()),
            PauseLength::UntilResumed => None,
        };
        CaptureState::Paused { until }
    }

    /// The state at `now`: a timed pause that has run out is running again.
    pub fn at<Tz: TimeZone>(self, now: &DateTime<Tz>) -> Self {
        match self {
            CaptureState::Paused { until: Some(until) } if now.to_utc() >= until => {
                CaptureState::Running
            }
            state => state,
        }
    }

    pub fn is_paused(self) -> bool {
        matches!(self, CaptureState::Paused { .. })
    }

    /// A short status line for the menu, in the time zone of `now`.
    pub fn label<Tz: TimeZone>(self, now: &DateTime<Tz>) -> String {
        match self.at(now) {
            CaptureState::Running => "Capture is on".to_string(),
            CaptureState::Paused { until: None } => "Paused until you resume".to_string(),
            CaptureState::Paused { until: Some(until) } => {
                let until = until.with_timezone(&now.timezone());
                let time = until.time().format("%H:%M");
                if until.date_naive() == now.date_naive() {
                    format!("Paused until {time}")
                } else if until.time() == NaiveTime::MIN {
                    "Paused until midnight".to_string()
                } else {
                    format!("Paused until {time} tomorrow")
                }
            }
        }
    }
}

/// The start of the next day in the time zone of `now`.
fn next_midnight<Tz: TimeZone>(now: &DateTime<Tz>) -> DateTime<Tz> {
    let tz = now.timezone();
    let tomorrow = now.date_naive().succ_opt();
    // In a few time zones the clocks change at midnight, so 00:00 may not exist.
    // Then the day starts at the first hour that does.
    tomorrow
        .and_then(|day| {
            (0..3).find_map(|hour| {
                tz.from_local_datetime(&day.and_hms_opt(hour, 0, 0)?)
                    .earliest()
            })
        })
        .unwrap_or_else(|| now.clone() + TimeDelta::days(1))
}

/// How often work in progress checks whether capture was paused meanwhile.
const PAUSE_CHECK: Duration = Duration::from_secs(1);

/// `work`'s output, or `None` once `stop` returns `true` (checked every
/// `every`), whichever comes first.
async fn until<T>(
    stop: impl Fn() -> bool,
    every: Duration,
    work: impl Future<Output = T>,
) -> Option<T> {
    let mut work = pin!(work);
    let mut check = tokio::time::interval(every);
    check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    poll_fn(|cx| {
        if stop() {
            return Poll::Ready(None);
        }
        if let Poll::Ready(output) = work.as_mut().poll(cx) {
            return Poll::Ready(Some(output));
        }
        // Wakes this future again at the next check.
        while check.poll_tick(cx).is_ready() {
            if stop() {
                return Poll::Ready(None);
            }
        }
        Poll::Pending
    })
    .await
}

/// The shared pause state of the running app.
#[derive(Default)]
pub struct Pause {
    state: Mutex<CaptureState>,
}

impl Pause {
    pub fn pause(&self, length: PauseLength) {
        self.set(CaptureState::paused(length, &Local::now()));
    }

    pub fn resume(&self) {
        self.set(CaptureState::Running);
    }

    /// The current state. A timed pause that has run out is cleared here.
    /// Capture asks before every element it reads, so this stays cheap: pause
    /// ends are compared in UTC, without a time zone lookup.
    pub fn state(&self) -> CaptureState {
        let mut state = self.lock();
        *state = state.at(&Utc::now());
        *state
    }

    /// Must be checked immediately before anything is read from the screen.
    pub fn is_paused(&self) -> bool {
        self.state().is_paused()
    }

    /// Runs `work`, such as a model call, unless capture is paused first.
    /// Then `work` is dropped, which for a model call closes the connection
    /// so the server can stop, and the result is `None`: pausing stops
    /// pending processing, not only reading (GUARDRAILS §4).
    pub async fn unless_paused<T>(&self, work: impl Future<Output = T>) -> Option<T> {
        until(|| self.is_paused(), PAUSE_CHECK, work).await
    }

    /// Takes over the state saved before Lax was last quit.
    pub fn restore(&self, saved: CaptureState) {
        self.set(saved);
    }

    fn set(&self, new: CaptureState) {
        *self.lock() = new;
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, CaptureState> {
        // The state is a plain value that is always written whole, so it can't
        // be left half-updated by a panic elsewhere.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::FixedOffset;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering::SeqCst};

    fn stockholm_summer() -> FixedOffset {
        FixedOffset::east_opt(2 * 3600).unwrap()
    }

    fn at(hour: u32, minute: u32) -> DateTime<FixedOffset> {
        stockholm_summer()
            .with_ymd_and_hms(2026, 6, 15, hour, minute, 0)
            .unwrap()
    }

    #[test]
    fn starts_running() {
        assert_eq!(CaptureState::default(), CaptureState::Running);
        assert!(!Pause::default().is_paused());
    }

    #[test]
    fn timed_pauses_end_after_their_length() {
        let now = at(14, 0);
        let fifteen = CaptureState::paused(PauseLength::FifteenMinutes, &now);
        assert!(fifteen.at(&at(14, 14)).is_paused());
        assert!(!fifteen.at(&at(14, 15)).is_paused());

        let hour = CaptureState::paused(PauseLength::OneHour, &now);
        assert!(hour.at(&at(14, 59)).is_paused());
        assert!(!hour.at(&at(15, 0)).is_paused());
    }

    #[test]
    fn pause_until_tomorrow_ends_at_local_midnight() {
        let state = CaptureState::paused(PauseLength::UntilTomorrow, &at(21, 30));
        assert!(state.at(&at(23, 59)).is_paused());
        let midnight = stockholm_summer()
            .with_ymd_and_hms(2026, 6, 16, 0, 0, 0)
            .unwrap();
        assert!(!state.at(&midnight).is_paused());
    }

    #[test]
    fn pause_until_tomorrow_shortly_after_midnight_lasts_the_day() {
        let state = CaptureState::paused(PauseLength::UntilTomorrow, &at(0, 5));
        assert!(state.at(&at(23, 59)).is_paused());
    }

    #[test]
    fn pause_until_resumed_never_ends_by_itself() {
        let state = CaptureState::paused(PauseLength::UntilResumed, &at(9, 0));
        let much_later = at(9, 0) + TimeDelta::days(365);
        assert!(state.at(&much_later).is_paused());
    }

    #[test]
    fn resume_ends_any_pause() {
        let pause = Pause::default();
        pause.pause(PauseLength::UntilResumed);
        assert!(pause.is_paused());
        pause.resume();
        assert!(!pause.is_paused());
    }

    #[test]
    fn a_new_pause_replaces_the_current_one() {
        let pause = Pause::default();
        pause.pause(PauseLength::UntilResumed);
        pause.pause(PauseLength::FifteenMinutes);
        assert!(matches!(
            pause.state(),
            CaptureState::Paused { until: Some(_) }
        ));
    }

    #[test]
    fn expired_pause_is_cleared() {
        let pause = Pause::default();
        pause.set(CaptureState::Paused {
            until: Some(Utc::now() - TimeDelta::seconds(1)),
        });
        assert_eq!(pause.state(), CaptureState::Running);
        assert!(!pause.is_paused());
    }

    #[test]
    fn labels_say_when_capture_resumes() {
        let now = at(14, 0);
        let label = |length| CaptureState::paused(length, &now).label(&now);
        assert_eq!(CaptureState::Running.label(&now), "Capture is on");
        assert_eq!(label(PauseLength::FifteenMinutes), "Paused until 14:15");
        assert_eq!(label(PauseLength::OneHour), "Paused until 15:00");
        assert_eq!(label(PauseLength::UntilTomorrow), "Paused until midnight");
        assert_eq!(label(PauseLength::UntilResumed), "Paused until you resume");

        let late = at(23, 30);
        let over_midnight = CaptureState::paused(PauseLength::OneHour, &late);
        assert_eq!(over_midnight.label(&late), "Paused until 00:30 tomorrow");
    }

    #[test]
    fn label_of_expired_pause_says_running() {
        let state = CaptureState::paused(PauseLength::FifteenMinutes, &at(14, 0));
        assert_eq!(state.label(&at(14, 30)), "Capture is on");
    }

    /// Sets a flag when dropped, to see that paused work is let go.
    struct Dropped(Arc<AtomicBool>);

    impl Drop for Dropped {
        fn drop(&mut self) {
            self.0.store(true, SeqCst);
        }
    }

    const CHECK: Duration = Duration::from_millis(5);

    #[tokio::test]
    async fn work_that_finishes_gives_its_output() {
        let work = async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            "episode"
        };
        assert_eq!(until(|| false, CHECK, work).await, Some("episode"));
    }

    #[tokio::test]
    async fn pausing_stops_work_in_progress() {
        let paused = Arc::new(AtomicBool::new(false));
        let dropped = Arc::new(AtomicBool::new(false));
        let guard = Dropped(dropped.clone());
        let work = async move {
            let _guard = guard;
            // A model that would take far longer than the test.
            tokio::time::sleep(Duration::from_secs(60)).await;
            "episode"
        };
        let flag = paused.clone();
        let pause_soon = async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            flag.store(true, SeqCst);
        };
        let started = std::time::Instant::now();
        let (output, ()) = tokio::join!(until(|| paused.load(SeqCst), CHECK, work), pause_soon,);
        assert_eq!(output, None);
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(dropped.load(SeqCst));
    }

    #[tokio::test]
    async fn nothing_starts_while_paused() {
        let started = Arc::new(AtomicBool::new(false));
        let flag = started.clone();
        let work = async move {
            flag.store(true, SeqCst);
        };
        assert_eq!(until(|| true, CHECK, work).await, None);
        assert!(!started.load(SeqCst));
    }

    #[tokio::test]
    async fn pause_ends_work_through_the_shared_state() {
        let pause = Pause::default();
        pause.pause(PauseLength::UntilResumed);
        assert_eq!(pause.unless_paused(async { 1 }).await, None);
        pause.resume();
        assert_eq!(pause.unless_paused(async { 1 }).await, Some(1));
    }
}
