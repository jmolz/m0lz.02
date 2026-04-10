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
//! | [`config`] | TOML configuration parsing (`.pice/config.toml`) |
//! | [`plan_parser`] | Markdown plan parsing, `## Contract` detection |
//! | [`provider`] | Provider registry lookup (path walking) |
//! | [`prompt`] | Pure prompt helpers (read_claude_md, get_git_diff) |
//! | [`paths`] | Path normalization helpers |
//! | [`protocol`] | Daemon RPC type definitions (not provider RPC) |
//! | [`cli`] | Shared `CommandRequest` / `CommandResponse` |
//! | [`transport`] | Socket path abstractions (Unix / Windows pipe) |

pub mod cli;
pub mod config;
pub mod paths;
pub mod plan_parser;
pub mod prompt;
pub mod protocol;
pub mod provider;
pub mod transport;
