//! Seam verification — static checks at layer boundaries.
//!
//! PRDv2 Feature 6. Implements the 12 empirically validated failure
//! categories (Google SRE 68% outage contribution study; Adyen 60K+
//! daily error analysis; Google Shepherd adaptive retry analysis).
//! The default registry contains at least one check per category.
//!
//! # Contract
//!
//! - Checks are **deterministic**: same input → same output, no clocks,
//!   no randomness, no global state.
//! - Checks are **fast**: <100ms each on realistic inputs. The seam runner
//!   enforces this budget via a timeout wrapper.
//! - Checks are **context-isolated**: they see only the boundary-filtered
//!   diff and boundary-scoped file set via [`SeamContext`] — never other
//!   layers' contracts, findings, or plan rationale.
//!
//! # Module layout
//!
//! | Module | Purpose |
//! |--------|---------|
//! | [`types`]    | Trait + context + result + finding + spec + boundary |
//! | [`registry`] | `Registry` — deterministic id→check map |
//! | [`defaults`] | 12 default check implementations, one per category |

pub mod defaults;
pub mod registry;
pub mod types;

pub use registry::{Registry, RegistryError};
pub use types::{
    LayerBoundary, ParseBoundaryError, SeamCheck, SeamCheckSpec, SeamContext, SeamFinding,
    SeamResult, BOUNDARY_SEP, BOUNDARY_SEP_ASCII, MAX_LAYER_NAME_LEN,
};

/// Build a registry pre-populated with every default check.
///
/// Populated by [`defaults::register_defaults`]. The registry is deterministic
/// — two calls in the same process always return the same `ids_in_order()`.
pub fn default_registry() -> Registry {
    let mut reg = Registry::new();
    // Phase 4.1 Pass-6 C13: this `expect` is a build-time invariant — the
    // defaults list is hand-curated and `register_defaults` only returns
    // Err on duplicate ids, which is checked by a unit test. A panic here
    // would be a deterministic build failure, not a runtime surprise.
    // Grandfathered under `-D clippy::expect_used`.
    #[allow(clippy::expect_used)]
    defaults::register_defaults(&mut reg)
        .expect("default registry construction must not produce duplicate ids");
    reg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_registry_is_deterministic() {
        let a = default_registry();
        let b = default_registry();
        assert_eq!(a.ids_in_order(), b.ids_in_order());
    }

    #[test]
    fn default_registry_covers_all_twelve_categories() {
        let reg = default_registry();
        let mut categories: Vec<u8> = reg.iter().map(|(_, c)| c.category()).collect();
        categories.sort();
        categories.dedup();

        // All 12 categories represented.
        for cat in 1..=12u8 {
            assert!(
                categories.contains(&cat),
                "default registry is missing a check for category {cat}; got {categories:?}"
            );
        }
    }

    #[test]
    fn every_default_check_has_category_in_range() {
        let reg = default_registry();
        for (id, check) in reg.iter() {
            let c = check.category();
            assert!(
                (1..=12).contains(&c),
                "check {id} reports out-of-range category {c}"
            );
        }
    }
}
