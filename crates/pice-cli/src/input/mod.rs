//! CLI-side input helpers — owned by the CLI adapter because the
//! daemon is headless.
//!
//! Phase 6 introduces interactive review-gate prompts; the daemon
//! NEVER talks to stdin / stderr directly because adapters other than
//! the CLI (Phase 7 dashboard, CI bot) will supply decisions through
//! different channels. All interactive logic lives here.

pub mod decision_source;
