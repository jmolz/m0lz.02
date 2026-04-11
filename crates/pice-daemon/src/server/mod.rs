//! Daemon RPC server — socket listener, transport impls, auth, router.
//!
//! Populated across T15–T18:
//! - T15: Unix socket transport (`server::unix`) ✅
//! - T16: Windows named pipe transport (`server::windows`)
//! - T17: authentication token generation + validation (`server::auth`)
//! - T18: RPC method dispatch table (`server::router`)
//!
//! Transport modules are `#[cfg]`-gated per platform: only the matching one
//! compiles on a given target. The orchestrator and router in T18+ will
//! consume the platform-appropriate listener through a small trait defined
//! here once the second impl lands.

#[cfg(unix)]
pub mod unix;
