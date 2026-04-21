//! Phase 7 Task 14: notifications config (user floor + project merge).
//!
//! Lives in `[notifications]` table of BOTH `~/.pice/config.toml` (user
//! floor) and `.pice/config.toml` (project override). Semantics match
//! the `workflow.review` floor-merge:
//!
//! > The project may DISABLE any `on_*` flag that the user enabled, but
//! > may NEVER ENABLE one the user disabled.
//!
//! Rationale: desktop notifications are a user-experience concern, not a
//! project concern. A user who muted `on_gate` (because they don't want
//! desktop popups during focused work) should not see them re-enabled
//! by a project's `.pice/config.toml`. The reverse — a project
//! disabling notifications that the user had enabled — is permitted
//! because project-specific noise (e.g., a flaky CI feature that
//! endlessly triggers gates) is a legitimate noise-reduction signal.

use serde::{Deserialize, Serialize};

/// Notification dispatcher config. All fields default to `true` so an
/// absent `[notifications]` table keeps notifications on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotificationsConfig {
    /// Emit on feature `Passed` terminal transition.
    #[serde(default = "default_true")]
    pub on_complete: bool,
    /// Emit on `GateRequested`.
    #[serde(default = "default_true")]
    pub on_gate: bool,
    /// Emit on `Failed` / `FailedInterrupted` / `Cancelled`.
    #[serde(default = "default_true")]
    pub on_failure: bool,
}

fn default_true() -> bool {
    true
}

impl Default for NotificationsConfig {
    fn default() -> Self {
        Self {
            on_complete: true,
            on_gate: true,
            on_failure: true,
        }
    }
}

impl NotificationsConfig {
    /// Floor-merge project config against user config.
    ///
    /// For each `on_*` flag: `result = user && project`. This enforces
    /// "project can DISABLE but never ENABLE" because the flag remains
    /// `false` if EITHER side has it off.
    ///
    /// Used by a Task 19 config-loader wiring (deferred) that reads
    /// `~/.pice/config.toml` + `.pice/config.toml` and floor-merges the
    /// two `[notifications]` tables before handing the result to
    /// `run_follow` / `run_wait`. Until the loader lands, the production
    /// paths use `NotificationsConfig::default()`; this function exists
    /// so the semantic is locked down by tests now and can be wired with
    /// one-line edits at each call site.
    #[allow(dead_code)]
    pub fn floor_merge(user: &Self, project: &Self) -> Self {
        Self {
            on_complete: user.on_complete && project.on_complete,
            on_gate: user.on_gate && project.on_gate,
            on_failure: user.on_failure && project.on_failure,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_all_true() {
        let c = NotificationsConfig::default();
        assert!(c.on_complete);
        assert!(c.on_gate);
        assert!(c.on_failure);
    }

    #[test]
    fn parses_from_toml_with_partial_fields() {
        // Partial configs should fill defaults for omitted keys.
        let toml_str = r#"
on_gate = false
"#;
        let parsed: NotificationsConfig = toml::from_str(toml_str).unwrap();
        assert!(parsed.on_complete);
        assert!(!parsed.on_gate);
        assert!(parsed.on_failure);
    }

    #[test]
    fn rejects_unknown_fields() {
        let toml_str = r#"
on_gate = true
bogus_field = 42
"#;
        let err = toml::from_str::<NotificationsConfig>(toml_str).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("bogus_field") || msg.contains("unknown field"),
            "expected unknown-field error, got: {msg}"
        );
    }

    #[test]
    fn floor_merge_disables_but_never_enables() {
        // User disables `on_gate`; project tries to re-enable → stays off.
        let user = NotificationsConfig {
            on_complete: true,
            on_gate: false,
            on_failure: true,
        };
        let project = NotificationsConfig {
            on_complete: true,
            on_gate: true, // project attempts to enable
            on_failure: true,
        };
        let merged = NotificationsConfig::floor_merge(&user, &project);
        assert!(merged.on_complete);
        assert!(
            !merged.on_gate,
            "user floor must hold — project cannot enable"
        );
        assert!(merged.on_failure);
    }

    #[test]
    fn floor_merge_allows_project_disable() {
        // User enables all; project disables `on_failure` → off.
        let user = NotificationsConfig::default();
        let project = NotificationsConfig {
            on_complete: true,
            on_gate: true,
            on_failure: false,
        };
        let merged = NotificationsConfig::floor_merge(&user, &project);
        assert!(merged.on_complete);
        assert!(merged.on_gate);
        assert!(!merged.on_failure, "project may disable what user enabled");
    }

    #[test]
    fn floor_merge_both_on_stays_on() {
        // Identity case — all-on + all-on → all-on.
        let c = NotificationsConfig::default();
        let merged = NotificationsConfig::floor_merge(&c, &c);
        assert_eq!(merged, c);
    }
}
