# P4 — MCP direct CRUD + bridge cleanup (D10 lockstep)

**Phase:** P4. Combined with the bridge cleanup that previously was P5 — D10 ratifies that they ship together (no mixed-deployment migration window in beta).
**Branch:** `feat/mcp-direct-and-bridge-cleanup`
**Worktree:** `../huly-kube-p4-direct`
**Depends on:** P1 (transport decision), P2 (`huly-client` crate), P3 (JWT broker).
**Blocks:** P6, P7.

## Goal

MCP talks **directly** to the Huly transactor. Delete the bridge HTTP gateway and all discovery plumbing.

After this PR:
- MCP gets JWTs from `huly.bridge.mint`, talks `huly_client::{Rest|Ws}HulyClient` directly to the transactor.
- Bridge has no `/api/v1/*`. No `huly.bridge.{announce,lookup,schema}` subjects.
- Bridge's only NATS surfaces: `huly.event.*` (transactor pushes) and `huly.bridge.mint` (req/reply).

## Scope

### MCP side

1. New `crates/huly-mcp/src/huly_client_factory.rs`:
   - `async fn for_workspace(nats, workspace, agent_id) -> Result<Arc<dyn HulyClient>>`.
   - Internally: `request_jwt` (from P3) → `RestHulyClient::new(jwt, transactor_url)` (or `WsHulyClient` per P1 outcome).
   - Caches per workspace in process memory, with refresh-before-expiry (`refresh_at_ms` from MintResponse).
2. Replace `bridge_client.rs` usage in `crates/huly-mcp/src/mcp/server.rs`. Each tool now:
   - Calls `huly_client_factory.for_workspace(...)` instead of `resolve_proxy_url + http_client.X`.
   - Uses the typed `HulyClient` trait directly.
3. **Delete entirely:**
   - `crates/huly-mcp/src/bridge_client.rs`
   - `crates/huly-mcp/src/discovery.rs`
   - All `huly.bridge.{announce,lookup,schema}` subscribers / lookups in MCP.
4. Config (`config/mcp.toml`):
   - Remove `[mcp] bridge_api_token`.
   - Add `[mcp] agent_id` (required; D8). Fail to start if unset and MCP host doesn't pass `clientInfo`.
   - Add `[mcp] transport = "rest"` or `"ws"` (per P1 outcome).

### Bridge side

1. **Delete entirely:**
   - `crates/huly-bridge/src/admin/` (whole directory: `router.rs`, `platform_api.rs`, etc.).
   - `crates/huly-bridge/src/bridge/announcer.rs`.
   - `crates/huly-bridge/src/bridge/schema_resolver.rs` (if MCP fetches schema directly via `loadModel`).
2. Remove the admin HTTP listener from startup (`main.rs` / `service/lifecycle.rs`).
3. Remove from `config/bridge.toml`:
   - `[admin] api_token`
   - `[admin] advertise_url` (the wildcard-host validator from #9 also goes — no longer relevant)
   - `[admin] host`, `[admin] port` (no HTTP server)
4. Remove `[admin]` section entirely from `config/bridge.example.toml` and `ansible/files/bridge.toml.example`.
5. Bridge `main.rs` retains: NATS connection, transactor WS, event forwarder, JWT mint responder. That's it.

### Shared types (`huly-common`)

Delete from `crates/huly-common/src/announcement.rs`:
- `BridgeAnnouncement`
- `LOOKUP_SUBJECT`
- `SCHEMA_FETCH_SUBJECT_PREFIX`
- `WorkspaceSchemaResponse`
- The wildcard-host helpers (`is_unspecified_host`, `extract_host`, `has_routable_proxy_url`) — irrelevant when nothing announces a routable URL anymore.

Keep:
- `ANNOUNCE_SUBJECT` only if `huly.event.*` namespace is still under `bridge.announce.*` — but since D3 ratifies new names, remove this too. Replace with `EVENT_SUBJECT_PREFIX = "huly.event"` if not already in place.

## Schema cache (D9)

In `huly_client_factory`, after building a client, MCP:
1. Fetches `loadModel` once (REST or WS per transport).
2. Caches `WorkspaceSchema` keyed by `(workspace, modelHash)`.
3. Subscribes to `huly.event.tx.core.class.*` etc. (filtered) — on any class/attribute mutation, invalidates the cache for that workspace.
4. TTL fallback: 5 minutes (refetch even without invalidation signal).
5. On `Hierarchy.findClass` or schema-mismatch error from a tool call: forced refetch.

## Tests

- MCP: `huly_client_factory` round-trips JWT acquisition + builds a working client (mock NATS + mock accounts).
- MCP: Tool calls go through new path (refactor existing tests).
- MCP: Schema cache invalidation on simulated `huly.event.tx.core.class.create`.
- MCP: TTL refresh after 5 minutes of inactivity.
- Bridge: smoke that startup no longer binds HTTP (admin module gone).
- E2E (manual, documented in PR): one `huly_delete` against muhasebot succeeds and emits the expected log lines.

## Acceptance

- [ ] All `mcp__huly__*` tools functional via direct path.
- [ ] `crates/huly-bridge/src/admin/` deleted.
- [ ] `crates/huly-mcp/src/{bridge_client,discovery}.rs` deleted.
- [ ] No traces of `huly.bridge.{announce,lookup,schema}` subjects in source (`grep -r`).
- [ ] `make clippy && make test` clean.
- [ ] Bridge LOC reduced ≥30% from `5edc6e3` baseline.
- [ ] E2E smoke documented in PR description.

## Risk: this is the biggest PR

Acceptable to split into 2 commits within one PR for review clarity (e.g., commit 1: MCP wires new path with bridge HTTP still active; commit 2: deletes bridge HTTP). Both commits must build and test green.

## Out

PR `feat: MCP direct CRUD + bridge cleanup`. Largest of the series. Reviewer should expect substantial deletion.
