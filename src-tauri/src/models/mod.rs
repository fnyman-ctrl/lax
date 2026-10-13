//! The model layer: one interface to any OpenAI-compatible server, local by
//! default (ADR 0004, ADR 0007, GUARDRAILS §3).

mod client;
mod endpoint;
pub(crate) mod structured;

use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use crate::pause::Pause;

pub use client::{ChatMessage, ModelClient, ModelError, Role};

/// Takes turns at the model: the journal, the memory and questions ask one
/// thing at a time, so a small local model isn't slowed down by two requests
/// at once (ADR 0008).
///
/// The user comes first. While a question waits or is answered, background
/// work doesn't start, and work already running is dropped (and done again
/// in a later round), so the user never waits for the journal.
/// Whether the chat model's server could be reached the last time Lax
/// tried, so the window can say why entries are waiting. Only in memory.
#[derive(Default)]
pub struct ModelHealth {
    /// When it was first found unreachable since it last answered, in
    /// milliseconds since 1970; 0 while it answers (or hasn't been tried).
    down_since: std::sync::atomic::AtomicI64,
}

impl ModelHealth {
    /// The server couldn't be reached at `now_ms`.
    pub fn down(&self, now_ms: i64) {
        let _ = self.down_since.compare_exchange(
            0,
            now_ms.max(1),
            std::sync::atomic::Ordering::Relaxed,
            std::sync::atomic::Ordering::Relaxed,
        );
    }

    /// The server answered.
    pub fn up(&self) {
        self.down_since
            .store(0, std::sync::atomic::Ordering::Relaxed);
    }

    /// Since when it has been unreachable, if it is.
    pub fn down_since(&self) -> Option<i64> {
        let since = self.down_since.load(std::sync::atomic::Ordering::Relaxed);
        (since != 0).then_some(since)
    }
}

#[derive(Default)]
pub struct ModelTurn {
    lock: tokio::sync::Mutex<()>,
    /// Questions waiting for, or holding, the model.
    interactive: AtomicUsize,
}

/// The model, held for a question the user waits on.
pub struct InteractiveTurn<'a> {
    _guard: tokio::sync::MutexGuard<'a, ()>,
    _counted: Counted<'a>,
}

/// Counts a question from when it starts waiting until it's done.
struct Counted<'a>(&'a AtomicUsize);

impl Drop for Counted<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// How often background work checks whether the user wants the model.
const YIELD_CHECK: Duration = Duration::from_millis(200);

impl ModelTurn {
    /// Waits for the model to be free, for a question the user waits on.
    /// Background work makes way at once.
    pub async fn take_interactive(&self) -> InteractiveTurn<'_> {
        self.interactive.fetch_add(1, Ordering::SeqCst);
        let counted = Counted(&self.interactive);
        let guard = self.lock.lock().await;
        InteractiveTurn {
            _guard: guard,
            _counted: counted,
        }
    }

    /// A question waits for, or holds, the model.
    fn wanted(&self) -> bool {
        self.interactive.load(Ordering::SeqCst) > 0
    }

    /// Runs background work in turn. `None` if capture was paused or the
    /// user asked something first: then the work was dropped, or never
    /// started. Callers must treat `None` like a pause: store nothing from
    /// it, and try again in a later round.
    pub async fn background<T>(&self, pause: &Pause, work: impl Future<Output = T>) -> Option<T> {
        let run = async {
            let _turn = self.lock.lock().await;
            work.await
        };
        crate::pause::until(|| pause.is_paused() || self.wanted(), YIELD_CHECK, run).await
    }
}
pub use endpoint::{Endpoint, Locality};
#[allow(unused_imports)] // Used by the journal (Stage 3).
pub use structured::{Captured, Entities, StructuredError, Validate, ask};

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    #[tokio::test]
    async fn a_question_comes_before_background_work() {
        let turn = Arc::new(ModelTurn::default());
        let pause = Arc::new(Pause::default());
        let dropped = Arc::new(AtomicBool::new(false));

        // Background work that would hold the model for a minute…
        struct Flag(Arc<AtomicBool>);
        impl Drop for Flag {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let flag = Flag(dropped.clone());
        let background = {
            let (turn, pause) = (turn.clone(), pause.clone());
            tokio::spawn(async move {
                turn.background(&pause, async move {
                    let _flag = flag;
                    tokio::time::sleep(Duration::from_secs(60)).await;
                })
                .await
            })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;

        // …makes way as soon as the user asks.
        let started = std::time::Instant::now();
        let held = turn.take_interactive().await;
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(background.await.unwrap(), None);
        assert!(dropped.load(Ordering::SeqCst));

        // While the question holds the model, background work doesn't start.
        assert_eq!(turn.background(&pause, async { 1 }).await, None);
        drop(held);
        assert_eq!(turn.background(&pause, async { 1 }).await, Some(1));
    }

    #[test]
    fn health_remembers_when_the_server_went_away() {
        let health = ModelHealth::default();
        assert_eq!(health.down_since(), None);
        health.down(1_000);
        // Later failures keep the first time.
        health.down(5_000);
        assert_eq!(health.down_since(), Some(1_000));
        health.up();
        assert_eq!(health.down_since(), None);
    }
}
