//! Context-assembly prompt builders for PICE commands.
//!
//! Populated in T13 — moved from `pice-cli/src/engine/prompt.rs`. The pure
//! helpers (`read_claude_md`, `get_git_diff`, etc.) stayed in
//! `pice-core::prompt::helpers` in T6.

pub mod builders;

pub use builders::{
    build_adversarial_prompt, build_commit_prompt, build_evaluate_prompt, build_execute_prompt,
    build_handoff_prompt, build_plan_prompt, build_prime_prompt, build_review_prompt,
};
