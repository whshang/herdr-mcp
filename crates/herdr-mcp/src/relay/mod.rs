//! Relay Protocol v1 — provider-independent pure foundation (Phase 1).
//!
//! This is the deterministic, provider-independent core of the Relay Protocol
//! v1 wire contract shared by the Cloudflare edge, the workstation `herdr-link`
//! sidecar, and any future VPS/Docker edge. It is a faithful port of the frozen
//! TypeScript oracle under `src/relay/` (`protocol.ts`, `validation.ts`,
//! `canonical-json.ts`, `errors.ts`, `contract.ts`) and preserves the wire ABI
//! exactly.
//!
//! Modules:
//!   canonical_json — deterministic canonical JSON (contract-hash input).
//!   contract       — contract manifest normalization, SHA-256 hash, shape check.
//!   errors         — delivery-state & retryability taxonomy + error codes.
//!   validation     — strict per-kind frame validation (unknown-field + bounds),
//!                    including the raw byte gate and canonical payload budgets.
//!
//! # Scoping note
//!
//! This is a PURE protocol/validation/contract/error foundation. It deliberately
//! owns no WebSocket client, daemon, launchd, keychain, runtime generation
//! manager, updater, native-host, extension, or Edge Worker code, and performs no
//! production mutation. It is compiled into the `herdr-mcp` binary crate, which
//! does not yet consume these APIs, so the whole module is marked
//! `#[allow(dead_code)]` at the crate level (not per-item) to keep `-D warnings`
//! clean while the transport integration lands in a later batch. Tests still run
//! and verify every public surface.

#![allow(dead_code)] // staged: no binary caller yet; exercised by unit tests

pub mod canonical_json;
pub mod contract;
pub mod errors;
pub mod validation;
