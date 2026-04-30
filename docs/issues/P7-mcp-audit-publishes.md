# P7 — `huly.mcp.*` audit publishes

**Phase:** P7 (implements D3 mcp side).
**Branch:** `feat/mcp-audit-events`
**Worktree:** `../huly-kube-p7-audit`
**Depends on:** P4 (direct MCP path).
**Blocks:** none.

## Goal

MCP publishes audit/intent events to NATS for external consumers. Bridge already publishes `huly.event.tx.*` (canonical). Together they form the join: subscribers correlate by `request_id` to see "what the AI tried" + "what actually happened".

## Subjects (recap from D3)

| Subject | Payload |
|---|---|
| `huly.mcp.tool.invoked` | `{tool, workspace, agent_id, params_digest, request_id, timestamp_ms}` |
| `huly.mcp.tool.completed` | `{request_id, ok\|err, duration_ms, result_digest\|error, timestamp_ms}` |
| `huly.mcp.action.{class}.{op}` | `{workspace, agent_id, request_id, target_id?, fields_changed?}` |
| `huly.mcp.error` | `{request_id, tool, code, message, params, transactor_request_id?}` |

`{class}.{op}` examples:
- `huly.mcp.action.tracker.issue.create`
- `huly.mcp.action.tracker.issue.delete`
- `huly.mcp.action.card.update`

## Scope

### `huly-mcp` source

1. Add publisher singleton:
   - `crates/huly-mcp/src/audit.rs`: `AuditPublisher { nats: async_nats::Client, agent_id: String }`.
   - Methods: `tool_invoked`, `tool_completed`, `action`, `error`.
   - Sync calls fire-and-forget (don't block tool path on NATS publish ack); errors logged at `warn!`.
2. Generate `request_id` at the entry of each `#[tool]` method. Use ULID (already monotonic, sortable, NATS-friendly).
3. Plumb `request_id` into:
   - `huly.mcp.tool.invoked` (immediately on entry).
   - `huly.mcp.tool.completed` (on return; `duration_ms` measured).
   - The transactor TX `meta.request_id` field (so `huly.event.tx.*` carries it through).
   - `huly.mcp.error` payload on failure path.
4. `params_digest` and `result_digest` = SHA-256 hex of the JSON body, **truncated to 16 chars** (collision-tolerant for audit, avoids leaking PII into NATS).
5. For each tool, emit `huly.mcp.action.{class}.{op}` after the canonical `tool.invoked` — only for mutating tools (`create`, `update`, `delete`, `create_issue`, `update_issue`, `create_card`, etc.). Reads don't get an action subject (use `tool.invoked` for read audit).

### Documentation

`docs/nats-subjects.md` — single source of truth for the subject taxonomy:
- All subjects (event side + mcp side).
- Wire schemas (JSON shape) for each.
- Stability guarantees ("subjects under `huly.event.*` are stable; `huly.mcp.*` may add fields but won't remove them in beta").

### Tests

- `audit.rs` unit: each method publishes the expected subject and payload shape.
- One integration test using `async-nats` test server: `huly_create` invocation produces 3 subjects in order (`tool.invoked` → `action.tracker.issue.create` → `tool.completed`).

## Out of scope

- JetStream stream for retention (D7 ratifies: not at v2 launch).
- Bidirectional control plane ("kill this agent's running tool"). Future.
- Subject-level access control (NATS ACLs are operator concern).

## Acceptance

- [ ] All 4 subject classes published from MCP for relevant tool calls.
- [ ] `request_id` (ULID) appears in MCP events and in transactor TX `meta.request_id`.
- [ ] `params_digest`/`result_digest` are 16-char truncated SHA-256, never raw bodies.
- [ ] `docs/nats-subjects.md` covers all subjects with schemas.
- [ ] `make clippy && make test` clean.
- [ ] Smoke: tail `nats sub 'huly.mcp.>'` during one `huly_create_issue`, observe all expected subjects.

## Out

PR `feat(huly-mcp): publish audit events to huly.mcp.*`.
