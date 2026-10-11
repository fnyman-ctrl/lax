//! The model layer: one interface to any OpenAI-compatible server, local by
//! default (ADR 0004, ADR 0007, GUARDRAILS §3).

mod client;
mod endpoint;
pub(crate) mod structured;

pub use client::{ChatMessage, ModelClient, ModelError, Role};

/// Takes turns at the chat model: the journal, the memory and questions ask
/// one thing at a time, so a small local model isn't slowed down by two
/// requests at once (ADR 0008). Waiting is first come, first served.
#[derive(Default)]
pub struct ModelTurn(tokio::sync::Mutex<()>);

impl ModelTurn {
    /// Waits for the model to be free; it is free again when the guard is
    /// dropped.
    pub async fn take(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.0.lock().await
    }
}
pub use endpoint::{Endpoint, Locality};
#[allow(unused_imports)] // Used by the journal (Stage 3).
pub use structured::{Captured, Entities, StructuredError, Validate, ask};
