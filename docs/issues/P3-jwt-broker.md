# P3 — JWT broker

**Phase:** P3 (implements D2).
**Branch:** `feat/jwt-broker`
**Worktree:** `../huly-kube-p3-jwt`
**Depends on:** P2 (uses `huly_client::accounts`).
**Blocks:** P4 (MCP needs JWTs to talk direct).

## Goal

Bridge mints workspace JWTs on demand over NATS req/reply. MCP fetches on cold start, caches in process memory, refreshes before expiry. Credentials never leave the bridge host.

## Scope

### Bridge side (`huly-bridge`)

Add subscriber on subject `huly.bridge.mint`:

**Request payload:**
```json
{
  "workspace": "muhasebot",
  "agent_id": "claude-code-murat-001",
  "request_id": "01HXG..."
}
```

**Response payload:**
```json
{
  "jwt": "eyJ...",
  "expires_at_ms": 1730000000000,
  "refresh_at_ms": 1729996400000,
  "transactor_url": "wss://huly.black.solutions/...",
  "rest_base_url": "https://huly.black.solutions/api/v1",
  "workspace_uuid": "0192abcd-...-..."
}
```

**Critical (per P1 spike):** `workspace_uuid` is the **REST URL key**, not the human-readable workspace slug. Path becomes `POST /api/v1/tx/{workspace_uuid}`, not `/api/v1/tx/{workspace_slug}`. Without this field, MCP cannot construct REST URLs. Source: `huly.core/packages/api-client/src/rest/rest.ts:110,267`. The slug → uuid mapping is determined by the bridge during `selectWorkspace` (it gets back a `WorkspaceLoginInfo` with `workspaceId` field — that's the uuid).

`refresh_at_ms` = `expires_at_ms - 60_000` (1 minute leeway). MCP refreshes when `now >= refresh_at_ms`.

Implementation:
- New module `crates/huly-bridge/src/bridge/mint_responder.rs`.
- Subscribes to `huly.bridge.mint`.
- For each request: call `huly_client::accounts::login` + `selectWorkspace` for the named workspace using bridge's stored credentials.
- Returns the response struct.
- Logs `agent_id` + `workspace` + `request_id` for audit. Does **not** log JWT body.

**Auth (which workspaces this bridge can mint for):**
- Bridge config gains `[[workspace_credentials]]` array: `{workspace, email, password}`. One credential entry per workspace this bridge governs.
- A request for a workspace not in the array returns `{error: "unknown_workspace"}`. No fallback to bridge's primary `[huly]` config (that's only the bridge's own session for events).

### Shared types (`huly-common`)

New module `crates/huly-common/src/mint.rs`:
- `MintRequest`, `MintResponse`, `MintError` types.
- Subject constant `pub const MINT_SUBJECT: &str = "huly.bridge.mint";`.
- Request timeout constant `pub const MINT_TIMEOUT: Duration = Duration::from_secs(5);`.

### MCP side (`huly-mcp`) — preparation only, not yet wired into tools

MCP can't use the broker yet (P4 wires it). For now, just add the **client helper** so P4 has it:
- `crates/huly-mcp/src/jwt_broker_client.rs`: async `request_jwt(nats, workspace, agent_id) -> Result<MintResponse>`.
- One unit test mocking NATS.

P4 will integrate this into the tool path (`HulyClient` factory).

## Tests

- Bridge: `mint_responder` happy path (mocks accounts client → returns canned JWT).
- Bridge: unknown workspace returns error.
- Bridge: accounts failure returns error with no panic.
- Bridge: JWT body never appears in logs (assert via `tracing-test` capture).
- MCP: `request_jwt` round-trips against an embedded NATS test server (or mock).

## P1 spike findings to honor

- **Workspace UUID** required (above). Broker returns it; MCP uses it for REST paths.
- **`huly_list_workspaces` is NOT served by the transactor.** It's served by the account service with a *different* token. P4 will need a separate path: account-service token cached at MCP cold start (or fetched from broker on demand). For P3, the broker should also be capable of returning an account-service token — extend `MintResponse` with optional `account_service_jwt` for the account-listing case, or define a sibling subject `huly.bridge.mint.account` for the account-service token. Decision: keep one subject, return both tokens when available.

Updated response shape:
```json
{
  "jwt": "eyJ...",                 // workspace token (transactor RPC)
  "account_service_jwt": "eyJ...", // account-service token (list_workspaces)
  "expires_at_ms": ...,
  "refresh_at_ms": ...,
  "transactor_url": "wss://huly.black.solutions/...",
  "rest_base_url": "https://huly.black.solutions/api/v1",
  "workspace_uuid": "0192abcd-..."
}
```

## Out of scope

- JWT refresh on the MCP side (cold-start fetch only — MCP runs are short).
- Per-agent ACL ("agent X may only mint for workspace Y"). Defer.
- Encryption of the response payload beyond NATS-level (NATS over TLS is operator's concern).

## Acceptance

- [ ] `huly.bridge.mint` subject responds with valid JWT for configured workspaces.
- [ ] Unknown workspace → structured error.
- [ ] JWT never appears in logs (verify in test).
- [ ] `make clippy && make test` clean.
- [ ] Documentation: comment block on `MINT_SUBJECT` describing wire format.

## Out

PR `feat(huly-bridge): JWT broker over huly.bridge.mint`. May span multiple commits if the broker + types + MCP client helper are easier reviewed separately. Each commit GPG-signed.
