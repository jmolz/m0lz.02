//! Registry of [`SeamCheck`] implementations, keyed by `id()`.
//!
//! Uses `BTreeMap` so iteration order is deterministic across runs — the seam
//! runner depends on this for reproducible output ordering and the Phase 3
//! contract (criterion #3) asserts determinism.

use super::types::SeamCheck;
use std::collections::BTreeMap;

/// Deterministic, lookup-by-id registry of seam checks.
pub struct Registry {
    checks: BTreeMap<String, Box<dyn SeamCheck + Send + Sync>>,
}

impl Registry {
    /// Build an empty registry. Use [`crate::seam::default_registry`] to get
    /// one pre-populated with every default check.
    pub fn new() -> Self {
        Self {
            checks: BTreeMap::new(),
        }
    }

    /// Register a check. Returns an error if the same `id()` is already
    /// registered — silent clobbering would make debugging "why is check X
    /// not running my logic" nearly impossible.
    pub fn register(
        &mut self,
        check: Box<dyn SeamCheck + Send + Sync>,
    ) -> Result<(), RegistryError> {
        let id = check.id().to_string();
        if self.checks.contains_key(&id) {
            return Err(RegistryError::DuplicateId(id));
        }
        self.checks.insert(id, check);
        Ok(())
    }

    /// Look up a check by id.
    pub fn get(&self, id: &str) -> Option<&(dyn SeamCheck + Send + Sync)> {
        self.checks.get(id).map(|b| b.as_ref())
    }

    /// True if a check with this id is registered.
    pub fn contains(&self, id: &str) -> bool {
        self.checks.contains_key(id)
    }

    /// Registered ids in deterministic (alphabetical) order.
    pub fn ids_in_order(&self) -> Vec<&str> {
        self.checks.keys().map(|s| s.as_str()).collect()
    }

    /// Iterate `(id, check)` pairs in deterministic order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &(dyn SeamCheck + Send + Sync))> {
        self.checks
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_ref() as &(dyn SeamCheck + Send + Sync)))
    }

    /// Number of registered checks.
    pub fn len(&self) -> usize {
        self.checks.len()
    }

    /// True if no checks are registered.
    pub fn is_empty(&self) -> bool {
        self.checks.is_empty()
    }
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}

/// Registry construction errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryError {
    DuplicateId(String),
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicateId(id) => write!(f, "seam check id '{id}' is already registered"),
        }
    }
}

impl std::error::Error for RegistryError {}

#[cfg(test)]
mod tests {
    use super::super::types::{LayerBoundary, SeamCheck, SeamContext, SeamResult};
    use super::*;
    use std::path::PathBuf;

    struct StubCheck {
        id_: &'static str,
        category_: u8,
    }

    impl SeamCheck for StubCheck {
        fn id(&self) -> &str {
            self.id_
        }
        fn category(&self) -> u8 {
            self.category_
        }
        fn applies_to(&self, _: &LayerBoundary) -> bool {
            true
        }
        fn run(&self, _: &SeamContext<'_>) -> SeamResult {
            SeamResult::Passed
        }
    }

    fn stub(id: &'static str, category: u8) -> Box<dyn SeamCheck + Send + Sync> {
        Box::new(StubCheck {
            id_: id,
            category_: category,
        })
    }

    #[test]
    fn register_and_get() {
        let mut reg = Registry::new();
        reg.register(stub("alpha", 1)).unwrap();
        assert_eq!(reg.len(), 1);
        assert_eq!(reg.get("alpha").unwrap().id(), "alpha");
        assert!(reg.contains("alpha"));
        assert!(!reg.contains("missing"));
    }

    #[test]
    fn duplicate_id_rejected() {
        let mut reg = Registry::new();
        reg.register(stub("alpha", 1)).unwrap();
        let err = reg.register(stub("alpha", 2)).unwrap_err();
        assert_eq!(err, RegistryError::DuplicateId("alpha".into()));
    }

    #[test]
    fn ids_iteration_order_is_deterministic() {
        // Register in a non-alphabetical order; BTreeMap sorts on insert.
        let mut a = Registry::new();
        a.register(stub("zeta", 1)).unwrap();
        a.register(stub("alpha", 2)).unwrap();
        a.register(stub("mu", 3)).unwrap();

        let mut b = Registry::new();
        b.register(stub("mu", 3)).unwrap();
        b.register(stub("zeta", 1)).unwrap();
        b.register(stub("alpha", 2)).unwrap();

        let ids_a: Vec<&str> = a.ids_in_order().to_vec();
        let ids_b: Vec<&str> = b.ids_in_order().to_vec();
        assert_eq!(ids_a, ids_b);
        assert_eq!(ids_a, vec!["alpha", "mu", "zeta"]);
    }

    #[test]
    fn iter_matches_ids_in_order() {
        let mut reg = Registry::new();
        reg.register(stub("beta", 1)).unwrap();
        reg.register(stub("alpha", 2)).unwrap();
        let ids_via_iter: Vec<&str> = reg.iter().map(|(id, _)| id).collect();
        assert_eq!(ids_via_iter, reg.ids_in_order());
    }

    #[test]
    fn empty_registry() {
        let reg = Registry::new();
        assert!(reg.is_empty());
        assert_eq!(reg.len(), 0);
        assert!(reg.get("anything").is_none());
    }

    // Exercise a stub through SeamContext to verify the context plumbing.
    #[test]
    fn stub_can_run_against_context() {
        let boundary = LayerBoundary::new("a", "b");
        let root = PathBuf::from("/tmp");
        let files: Vec<PathBuf> = Vec::new();
        let ctx = SeamContext {
            boundary: &boundary,
            filtered_diff: "",
            repo_root: &root,
            boundary_files: &files,
            args: None,
        };
        let check = StubCheck {
            id_: "alpha",
            category_: 1,
        };
        assert!(check.run(&ctx).is_passed());
    }
}
