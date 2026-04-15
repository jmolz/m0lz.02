//! Default seam-check library — 12 checks, one per PRDv2 failure category.
//!
//! Each submodule implements a single [`SeamCheck`]. The aggregator below
//! registers all of them into a fresh [`Registry`]. Adding a new default
//! check requires: (a) a new submodule, (b) a `mod` line here, (c) a
//! `register_defaults` entry. The default-registry tests in
//! `crate::seam::tests` enforce that every category 1..=12 has at least
//! one representative.

use super::registry::{Registry, RegistryError};

pub(crate) mod env_scan;

pub mod auth_handoff;
pub mod cascade_timeout;
pub mod cold_start_order;
pub mod config_mismatch;
pub mod health_check;
pub mod network_topology;
pub mod openapi_compliance;
pub mod resource_exhaustion;
pub mod retry_storm;
pub mod schema_drift;
pub mod service_discovery;
pub mod version_skew;

/// Register every default check into `registry`. Order is irrelevant for
/// correctness — `Registry` keeps ids sorted.
pub fn register_defaults(registry: &mut Registry) -> Result<(), RegistryError> {
    registry.register(Box::new(config_mismatch::ConfigMismatchCheck))?;
    registry.register(Box::new(version_skew::VersionSkewCheck))?;
    registry.register(Box::new(openapi_compliance::OpenApiComplianceCheck))?;
    registry.register(Box::new(auth_handoff::AuthHandoffCheck))?;
    registry.register(Box::new(cascade_timeout::CascadeTimeoutCheck))?;
    registry.register(Box::new(retry_storm::RetryStormCheck))?;
    registry.register(Box::new(service_discovery::ServiceDiscoveryCheck))?;
    registry.register(Box::new(health_check::HealthCheckCheck))?;
    registry.register(Box::new(schema_drift::SchemaDriftCheck))?;
    registry.register(Box::new(cold_start_order::ColdStartOrderCheck))?;
    registry.register(Box::new(network_topology::NetworkTopologyCheck))?;
    registry.register(Box::new(resource_exhaustion::ResourceExhaustionCheck))?;
    Ok(())
}
