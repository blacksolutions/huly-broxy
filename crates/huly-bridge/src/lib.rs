//! Library facade for `huly-bridge`.
//!
//! The crate's primary artifact is the `huly-bridge` binary (see `main.rs`),
//! but exposing the same modules via a `lib` target lets integration tests
//! under `tests/` exercise internals (REST client, RPC types, etc.) without
//! re-implementing them. The binary keeps full ownership of process startup;
//! this library is a thin re-export layer with no runtime behaviour.
//!
//! Added during Phase 2 to unblock end-to-end REST tests against the
//! `MockHuly` harness.

#![allow(dead_code)]

pub mod admin;
pub mod bridge;
pub mod config;
pub mod error;
pub mod huly;
pub mod service;
