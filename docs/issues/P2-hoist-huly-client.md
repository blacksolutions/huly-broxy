# P2 — Hoist `huly-client` crate

**Phase:** P2.
**Branch:** `refactor/hoist-huly-client`
**Worktree:** `../huly-kube-p2-hoist`
**Depends on:** none.
**Blocks:** P3 (JWT broker uses shared types), P4 (MCP imports the client).

## Goal

Move all transactor-protocol code out of `huly-bridge` into a new `huly-client` crate so both `huly-bridge` and `huly-mcp` can depend on it.

**Pure mechanical move. No behavior change. No new types.** If anything reshapes, it goes in a follow-up PR.

## Scope

Create `crates/huly-client/` with:

```
crates/huly-client/
├── Cargo.toml
└── src/
    ├── lib.rs
    ├── accounts.rs      ← from huly-bridge/src/huly/accounts.rs
    ├── client.rs        ← from huly-bridge/src/huly/client.rs
    ├── connection.rs    ← from huly-bridge/src/huly/connection.rs
    ├── proxy.rs         ← from huly-bridge/src/huly/proxy.rs
    ├── rpc.rs           ← from huly-bridge/src/huly/rpc.rs
    ├── schema_resolver.rs ← from huly-bridge/src/bridge/schema_resolver.rs
    └── types.rs         ← from huly-bridge/src/huly/types.rs (if exists)
```

`huly-bridge` re-exports via `pub use huly_client::*;` for paths that imports rely on, OR rewrites imports — prefer the second (cleaner, the few callsites in bridge are easy to update).

## Procedure

1. `cd /mnt/workspace/server-yazilim/huly-kube && git worktree add ../huly-kube-p2-hoist refactor/hoist-huly-client`
2. In the worktree:
   - `mkdir -p crates/huly-client/src`
   - `git mv crates/huly-bridge/src/huly/*.rs crates/huly-client/src/` (preserves blame).
   - Move `crates/huly-bridge/src/bridge/schema_resolver.rs` to `crates/huly-client/src/schema_resolver.rs` (this is technically protocol-level — it reads from transactor; bridge supervises but doesn't define).
   - Write `crates/huly-client/Cargo.toml` matching `huly-bridge`'s deps for these files (`async-nats`, `reqwest`, `serde`, `serde_json`, `tokio`, `tokio-tungstenite`, `tracing`, etc.).
   - Write `crates/huly-client/src/lib.rs`: `pub mod accounts; pub mod client; pub mod connection; pub mod rpc; pub mod schema_resolver; pub mod types;`
   - Update root `Cargo.toml` workspace members.
   - Update `crates/huly-bridge/Cargo.toml` to depend on `huly-client = { path = "../huly-client" }`.
   - In `huly-bridge`, replace `crate::huly::*` with `huly_client::*`. Replace `crate::bridge::schema_resolver` references with `huly_client::schema_resolver`.
   - Delete `crates/huly-bridge/src/huly/mod.rs` (or rewrite as empty if there's other content).
3. `make clippy && make test` — must pass without functional changes.
4. Commit: `refactor(huly-client): hoist transactor protocol into shared crate`. Single commit, GPG-signed.

## Out of scope

- Renaming types or methods.
- Splitting `client.rs` into smaller files.
- Changing the `HulyClient` trait surface.
- Adding `RestHulyClient` (that's P4 — informed by P1 spike).

## Acceptance

- [ ] `crates/huly-client/` exists with the moved files; git blame preserved (`git log --follow` works).
- [ ] `crates/huly-bridge/src/huly/` deleted (no leftover module).
- [ ] `Cargo.toml` workspace lists three crates: `huly-client`, `huly-bridge`, `huly-mcp`, plus existing `huly-common`.
- [ ] `make clippy` clean.
- [ ] `make test` green — same test count as before (pure move).
- [ ] No new public API; existing `RpcError`, `HulyClient` trait, `WorkspaceSchema` etc. are at their new paths.

## Out

PR titled `refactor(huly-client): hoist transactor protocol into shared crate`. Single commit. No semantic change.
