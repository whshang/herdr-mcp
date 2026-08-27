//! Staged workstation-link reliability and transport foundation.
//!
//! The reliability kernels are transport-independent. `transport` adds the
//! first socket-event reactor above them while still owning no credential,
//! concrete WebSocket implementation, runtime dispatch, launchd, runtime
//! install, or production cutover.
//!
//! `backoff` mirrors the deterministic reconnect-delay policy.
//! `generation_fence` tracks which runtime generation actually owns each
//! in-flight request so activation cannot relabel an old completion as the new
//! active generation.
//!
//! The generation fence is NOT the durable service-generation ledger in
//! `state_store`; it is link-local request ownership state only.

#![allow(dead_code)] // staged until the Rust link transport consumes it

pub mod backoff;
pub mod cutover;
pub mod daemon;
pub mod generation_fence;
pub mod heartbeat;
pub mod install;
pub mod io_loop;
pub mod lifecycle;
pub mod local_mcp;
pub mod migrate_runtime_control;
pub mod ownership;
pub mod policy;
pub mod request_core;
pub mod run;
pub mod runner;
pub mod runtime_control;
pub mod runtime_generation;
pub mod socket_driver;
pub mod transport;

pub use cutover::{CutoverMode, run as run_link_cutover};
pub use install::{
    LINK_RUST_CANDIDATE_LABEL, install as run_link_install, uninstall as run_link_uninstall,
};
pub use migrate_runtime_control::{MigrateMode, run as run_link_migrate_runtime_control};
pub use ownership::{
    doctor_layer_summary, production_ready_gate_catalog, run_status as run_link_status,
};
pub use run::{LINK_RUN_WIRED, run as run_link};
