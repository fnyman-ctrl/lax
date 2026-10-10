//! The model layer: one interface to any OpenAI-compatible server, local by
//! default (ADR 0004, ADR 0007, GUARDRAILS §3).

mod client;
mod endpoint;

pub use client::{ChatMessage, ModelClient, Role};
pub use endpoint::{Endpoint, Locality};
