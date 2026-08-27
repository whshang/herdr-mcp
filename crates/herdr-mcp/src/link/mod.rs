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
pub mod daemon;
pub mod generation_fence;
pub mod heartbeat;
pub mod io_loop;
pub mod lifecycle;
pub mod local_mcp;
pub mod ownership;
pub mod policy;
pub mod request_core;
pub mod runner;
pub mod runtime_control;
pub mod runtime_generation;
pub mod socket_driver;
pub mod transport;

pub use ownership::{
    doctor_layer_summary, production_ready_gate_catalog, run_status as run_link_status,
};
