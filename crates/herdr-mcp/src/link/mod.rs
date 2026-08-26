//! Staged workstation-link reliability foundation.
//!
//! This module ports only transport-independent reliability state from the
//! existing Node `src/link/**` implementation. It intentionally owns no socket,
//! credential, timer, HTTP, launchd, runtime install, or production cutover.
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
pub mod generation_fence;
pub mod heartbeat;
pub mod lifecycle;
pub mod policy;
