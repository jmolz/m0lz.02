//! # pice-core
//!
//! Shared pure-logic crate consumed by both `pice-cli` (thin CLI adapter) and
//! `pice-daemon` (long-running orchestrator). Owns configuration parsing, plan
//! parsing, provider registry lookup, path helpers, pure prompt helpers, the
//! shared `CommandRequest`/`CommandResponse` enums, the daemon RPC type
//! definitions, and the platform-abstracted transport descriptors.
//!
//! ## Invariants
//!
//! - Zero async dependencies (no `tokio`).
//! - Zero network dependencies (no `reqwest`).
//! - Zero database dependencies (no `rusqlite`).
//! - All types are serde-friendly where they cross the daemon RPC boundary.
//!
//! Graded by contract criterion #1 of the Phase 0 daemon-foundation plan.
//!
//! ## Module map
//!
//! | Module | Purpose |
//! |--------|---------|
//! | [`adaptive`] | SPRT/ADTS/VEC algorithms + halt dispatcher (PRDv2 Feature 7) |
//! | [`config`] | TOML configuration parsing (`.pice/config.toml`) |
//! | [`events`] | Phase 7 `ManifestEvent` / `LogChunk` wire types |
//! | [`jobs`] | Phase 7 `JobEnv` dispatch-time env snapshot |
//! | [`plan_parser`] | Markdown plan parsing, `## Contract` detection |
//! | [`provider`] | Provider registry lookup (path walking) |
//! | [`prompt`] | Pure prompt helpers (read_claude_md, get_git_diff) |
//! | [`paths`] | Path normalization helpers |
//! | [`protocol`] | Daemon RPC type definitions (not provider RPC) |
//! | [`cli`] | Shared `CommandRequest` / `CommandResponse` |
//! | [`layers`] | Layer types, detection, diff filtering, manifest |
//! | [`transport`] | Socket path abstractions (Unix / Windows pipe) |

pub mod adaptive;
pub mod cli;
pub mod config;
pub mod events;
pub mod gate;
pub mod jobs;
pub mod layers;
pub mod memory;
pub mod paths;
pub mod plan_parser;
pub mod prompt;
pub mod protocol;
pub mod provider;
pub mod seam;
pub mod transport;
pub mod workflow;
