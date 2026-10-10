//! The model layer: one interface to any OpenAI-compatible server, local by
//! default (ADR 0004, ADR 0007, GUARDRAILS §3).

mod client;
mod endpoint;
mod structured;

pub use client::{ChatMessage, ModelClient, Role};
pub use endpoint::{Endpoint, Locality};
#[allow(unused_imports)] // Used by the journal (Stage 3).
pub use structured::{Captured, Entities, StructuredError, Validate, ask};
