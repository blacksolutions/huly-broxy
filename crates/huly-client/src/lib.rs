//! Huly transactor protocol client.
//!
//! Hoisted from `huly-bridge` so both the bridge and the MCP server can
//! depend on the same wire-protocol code. Pure mechanical lift — no API
//! changes, no new types. See `docs/issues/P2-hoist-huly-client.md`.

pub mod accounts;
pub mod auth;
pub mod client;
pub mod collaborator;
pub mod connection;
pub mod markdown;
pub mod proxy;
pub mod rate_limit;
pub mod rest;
pub mod rpc;
pub mod schema_resolver;
pub mod types;
