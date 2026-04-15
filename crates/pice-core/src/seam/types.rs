//! Core seam types: trait, context, result, boundary, finding, spec.
//!
//! All seam check implementations live under [`crate::seam::defaults`]. This
//! module only defines the abstractions they share.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

// ─── Boundary ───────────────────────────────────────────────────────────────

/// A boundary between two layers, normalized to alphabetical order so
/// `LayerBoundary::new("api", "backend")` and `LayerBoundary::new("backend",
/// "api")` compare equal. Storage form is always `{a}↔{b}` with `a <= b`.
///
/// User-facing parsing accepts both `"A↔B"` and `"A<->B"` via
/// [`LayerBoundary::parse`] and canonicalizes to `↔`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LayerBoundary {
    /// Canonical first layer name (alphabetically earlier of the pair).
    pub a: String,
    /// Canonical second layer name (alphabetically later of the pair).
    pub b: String,
}

/// Canonical seam-boundary separator. Unicode arrow reads well in docs and
/// error messages. Parsers accept `<->` and `↔`; canonical form is always `↔`.
pub const BOUNDARY_SEP: &str = "↔";

/// ASCII-safe alternative separator accepted by [`LayerBoundary::parse`].
pub const BOUNDARY_SEP_ASCII: &str = "<->";

/// Maximum accepted layer-name length for boundary parsing. Keeps error
/// messages and storage bounded against pathological user input.
pub const MAX_LAYER_NAME_LEN: usize = 128;

impl LayerBoundary {
    /// Construct a boundary, canonicalizing so `a <= b` alphabetically.
    pub fn new(x: impl Into<String>, y: impl Into<String>) -> Self {
        let (x, y) = (x.into(), y.into());
        if x <= y {
            Self { a: x, b: y }
        } else {
            Self { a: y, b: x }
        }
    }

    /// Parse `"A↔B"` or `"A<->B"`. Rejects empty sides, `A == B`, and
    /// multi-separator strings like `"A↔B↔C"`.
    pub fn parse(s: &str) -> Result<Self, ParseBoundaryError> {
        // Accept either separator; detect which is present.
        let (lhs, rhs) = if let Some((l, r)) = s.split_once(BOUNDARY_SEP) {
            (l, r)
        } else if let Some((l, r)) = s.split_once(BOUNDARY_SEP_ASCII) {
            (l, r)
        } else {
            return Err(ParseBoundaryError::MissingSeparator(s.to_string()));
        };

        // Reject any further separators — boundaries are binary.
        if lhs.contains(BOUNDARY_SEP)
            || lhs.contains(BOUNDARY_SEP_ASCII)
            || rhs.contains(BOUNDARY_SEP)
            || rhs.contains(BOUNDARY_SEP_ASCII)
        {
            return Err(ParseBoundaryError::TooManySeparators(s.to_string()));
        }

        let a = lhs.trim();
        let b = rhs.trim();
        if a.is_empty() || b.is_empty() {
            return Err(ParseBoundaryError::EmptySide(s.to_string()));
        }
        if a == b {
            return Err(ParseBoundaryError::SelfBoundary(a.to_string()));
        }
        if a.len() > MAX_LAYER_NAME_LEN || b.len() > MAX_LAYER_NAME_LEN {
            return Err(ParseBoundaryError::NameTooLong {
                raw: s.to_string(),
                limit: MAX_LAYER_NAME_LEN,
            });
        }

        Ok(Self::new(a, b))
    }

    /// Canonical on-disk / error-message form: `{a}↔{b}`.
    pub fn canonical(&self) -> String {
        format!("{}{}{}", self.a, BOUNDARY_SEP, self.b)
    }

    /// True iff the boundary's canonical key matches `raw` exactly.
    pub fn matches_raw(&self, raw: &str) -> bool {
        raw == self.canonical()
    }

    /// True iff the boundary touches the given layer (appears on either side).
    pub fn touches(&self, layer: &str) -> bool {
        self.a == layer || self.b == layer
    }

    /// The non-`layer` side of the boundary, if `layer` is one side.
    pub fn other(&self, layer: &str) -> Option<&str> {
        if self.a == layer {
            Some(&self.b)
        } else if self.b == layer {
            Some(&self.a)
        } else {
            None
        }
    }
}

/// Errors surfaced by [`LayerBoundary::parse`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseBoundaryError {
    MissingSeparator(String),
    TooManySeparators(String),
    EmptySide(String),
    SelfBoundary(String),
    NameTooLong { raw: String, limit: usize },
}

impl std::fmt::Display for ParseBoundaryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingSeparator(s) => {
                write!(f, "seam boundary '{s}' must contain '↔' or '<->'")
            }
            Self::TooManySeparators(s) => write!(
                f,
                "seam boundary '{s}' has more than one separator; boundaries are binary"
            ),
            Self::EmptySide(s) => {
                write!(f, "seam boundary '{s}' has an empty layer name on one side")
            }
            Self::SelfBoundary(n) => write!(
                f,
                "seam boundary references the same layer on both sides: '{n}↔{n}'"
            ),
            Self::NameTooLong { raw, limit } => write!(
                f,
                "seam boundary '{raw}' has a layer name exceeding {limit} characters"
            ),
        }
    }
}

impl std::error::Error for ParseBoundaryError {}

// ─── Check spec (wire + storage form) ───────────────────────────────────────

/// Per-boundary user-supplied check specification. Carried through the
/// provider protocol (`evaluate/create.seamChecks`) and persisted in workflow
/// YAML. `id` must match a registered check's `id()`.
///
/// `boundary` is optional on the protocol side so providers that run
/// boundary-agnostic checks can omit it; the daemon always supplies it in
/// practice (it's the canonical `"A↔B"` key).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SeamCheckSpec {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boundary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<BTreeMap<String, serde_json::Value>>,
}

// ─── Check context ──────────────────────────────────────────────────────────

/// Runtime context exposed to a seam check's `run()`.
///
/// # Context isolation
///
/// `SeamContext` is deliberately narrow. It only exposes data scoped to the
/// boundary being checked — NEVER other layers' contracts, findings, plan
/// rationale, or cross-layer diffs. The `Debug` derive is intentionally
/// omitted so accidental `{:?}` logging cannot leak isolation-violating data.
/// See the `context_isolation_leak` test in the seam runner.
pub struct SeamContext<'a> {
    /// The boundary this context was prepared for.
    pub boundary: &'a LayerBoundary,
    /// Unified git diff filtered to files in either side of the boundary.
    pub filtered_diff: &'a str,
    /// Repository root for reads that require filesystem access.
    pub repo_root: &'a Path,
    /// Concrete file set defining the boundary — the union of files touched
    /// in either `a` or `b` layers, relative to `repo_root`. Checks should
    /// treat this list as the only allowed read set.
    pub boundary_files: &'a [PathBuf],
    /// Check-specific arguments forwarded from [`SeamCheckSpec::args`].
    /// Always `None` in v0.2 (no default check consumes args yet) but
    /// plumbed through so plugin crates may consume structured args.
    pub args: Option<&'a BTreeMap<String, serde_json::Value>>,
}

// ─── Check result ───────────────────────────────────────────────────────────

/// Outcome of running a single seam check.
///
/// `Passed` means "no finding at this boundary". `Warning` surfaces findings
/// but does NOT fail the layer (per PRDv2 line 1038: warnings are advisory).
/// `Failed` is fail-closed — the layer will be marked `LayerStatus::Failed`.
#[derive(Debug, Clone, PartialEq)]
pub enum SeamResult {
    Passed,
    Warning(Vec<SeamFinding>),
    Failed(Vec<SeamFinding>),
}

impl SeamResult {
    pub fn is_passed(&self) -> bool {
        matches!(self, Self::Passed)
    }

    pub fn is_warning(&self) -> bool {
        matches!(self, Self::Warning(_))
    }

    pub fn is_failed(&self) -> bool {
        matches!(self, Self::Failed(_))
    }

    pub fn findings(&self) -> &[SeamFinding] {
        match self {
            Self::Passed => &[],
            Self::Warning(f) | Self::Failed(f) => f,
        }
    }
}

/// A single boundary finding. `file` and `line` are best-effort source
/// locators; checks without precise spans may leave them `None`.
#[derive(Debug, Clone, PartialEq)]
pub struct SeamFinding {
    pub message: String,
    pub file: Option<PathBuf>,
    pub line: Option<u32>,
}

impl SeamFinding {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            file: None,
            line: None,
        }
    }

    pub fn with_file(mut self, file: impl Into<PathBuf>) -> Self {
        self.file = Some(file.into());
        self
    }

    pub fn with_line(mut self, line: u32) -> Self {
        self.line = Some(line);
        self
    }
}

// ─── Trait ──────────────────────────────────────────────────────────────────

/// A deterministic, static verification applied at a single layer boundary.
///
/// Implementations MUST:
/// - Be deterministic — same input → same output, no clocks, no randomness.
/// - Complete in <100ms on realistic inputs. The seam runner enforces this
///   budget and emits a `Warning` on timeout rather than panicking.
/// - Read only via [`SeamContext::filtered_diff`] and files referenced by
///   [`SeamContext::boundary_files`]. No global filesystem or network access.
/// - Never reach into other layers' contracts, findings, or plan rationale —
///   see the context-isolation rule in `.claude/rules/stack-loops.md`.
pub trait SeamCheck: Send + Sync {
    /// Stable identifier used in `workflow.yaml [seams]` and the registry.
    fn id(&self) -> &str;

    /// PRDv2 failure category (1..=12). Surfaces to the SQLite
    /// `seam_findings.category` column and the JSON output.
    fn category(&self) -> u8;

    /// True if this check applies to a given boundary. Checks return `false`
    /// for boundaries they don't recognize; the runner will skip them.
    fn applies_to(&self, boundary: &LayerBoundary) -> bool;

    /// Run the check against `ctx`. Must be deterministic and <100ms.
    fn run(&self, ctx: &SeamContext<'_>) -> SeamResult;
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boundary_canonicalizes_alphabetically() {
        let a = LayerBoundary::new("api", "backend");
        let b = LayerBoundary::new("backend", "api");
        assert_eq!(a, b);
        assert_eq!(a.canonical(), "api↔backend");
    }

    #[test]
    fn boundary_parse_accepts_unicode_and_ascii() {
        let unicode = LayerBoundary::parse("api↔backend").unwrap();
        let ascii = LayerBoundary::parse("api<->backend").unwrap();
        assert_eq!(unicode, ascii);
        assert_eq!(unicode.canonical(), "api↔backend");
    }

    #[test]
    fn boundary_parse_trims_whitespace() {
        let b = LayerBoundary::parse("  api  <->  backend  ").unwrap();
        assert_eq!(b.canonical(), "api↔backend");
    }

    #[test]
    fn boundary_parse_rejects_empty_side() {
        assert!(matches!(
            LayerBoundary::parse("↔backend").unwrap_err(),
            ParseBoundaryError::EmptySide(_)
        ));
        assert!(matches!(
            LayerBoundary::parse("backend↔").unwrap_err(),
            ParseBoundaryError::EmptySide(_)
        ));
    }

    #[test]
    fn boundary_parse_rejects_self_reference() {
        assert!(matches!(
            LayerBoundary::parse("api↔api").unwrap_err(),
            ParseBoundaryError::SelfBoundary(_)
        ));
    }

    #[test]
    fn boundary_parse_rejects_missing_separator() {
        assert!(matches!(
            LayerBoundary::parse("apibackend").unwrap_err(),
            ParseBoundaryError::MissingSeparator(_)
        ));
    }

    #[test]
    fn boundary_parse_rejects_three_way() {
        assert!(matches!(
            LayerBoundary::parse("api↔backend↔database").unwrap_err(),
            ParseBoundaryError::TooManySeparators(_)
        ));
    }

    #[test]
    fn boundary_touches_and_other() {
        let b = LayerBoundary::new("api", "backend");
        assert!(b.touches("api"));
        assert!(b.touches("backend"));
        assert!(!b.touches("frontend"));
        assert_eq!(b.other("api"), Some("backend"));
        assert_eq!(b.other("backend"), Some("api"));
        assert_eq!(b.other("frontend"), None);
    }

    #[test]
    fn seam_check_spec_denies_unknown_fields() {
        let bad = r#"{"id":"x","bogus":1}"#;
        let res: Result<SeamCheckSpec, _> = serde_json::from_str(bad);
        assert!(res.is_err(), "unknown field should be rejected");
    }

    #[test]
    fn seam_check_spec_roundtrip_without_args() {
        let spec = SeamCheckSpec {
            id: "config_mismatch".into(),
            boundary: None,
            args: None,
        };
        let json = serde_json::to_string(&spec).unwrap();
        // args = None must be omitted from the wire form.
        assert!(!json.contains("args"), "None args should be skipped: {json}");
        let back: SeamCheckSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(back, spec);
    }

    #[test]
    fn seam_result_helpers() {
        assert!(SeamResult::Passed.is_passed());
        assert!(SeamResult::Warning(vec![SeamFinding::new("x")]).is_warning());
        assert!(SeamResult::Failed(vec![SeamFinding::new("x")]).is_failed());
        assert_eq!(SeamResult::Passed.findings().len(), 0);
    }

    #[test]
    fn seam_finding_builder() {
        let f = SeamFinding::new("drift")
            .with_file("schema.prisma")
            .with_line(12);
        assert_eq!(f.message, "drift");
        assert_eq!(f.line, Some(12));
        assert_eq!(f.file.unwrap().to_string_lossy(), "schema.prisma");
    }
}
