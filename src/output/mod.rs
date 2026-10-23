//! Centralized output presentation.
//!
//! One run installs a single scoped [`context::OutputContext`] that carries the
//! resolved output format, the human view of the command, and the terminal
//! preferences. Every emitter reads that scope instead of deciding a format on
//! its own, so the machine JSON contract and the human text rendering can
//! never disagree about which channel a result belongs to.
//!
//! The scope is thread-local and RAII, so nested runs restore the previous
//! state and parallel tests never observe each other's context.

pub mod context;
pub mod human;
pub mod progress;
pub mod render;

#[cfg(test)]
mod tests;
