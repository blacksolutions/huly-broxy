# P1 — REST spike for MCP→transactor

**Phase:** P1 (decides D1).
**Branch:** `spike/rest-from-mcp`
**Worktree:** `../huly-kube-p1-rest-spike`
**Depends on:** P0 not strictly required, but nice to have for clear error surfacing during the spike.
**Blocks:** P4 (which transport MCP uses).

## Goal

Decide D1 with evidence: **does REST suffice for the full MCP tool surface, or does MCP need a per-workspace WS?**

This is exploratory. Output is a **spike report** (`docs/research/p1-rest-spike-report.md`), not a polished PR. Code is allowed to be ugly; tests can be smoke-only.

## Probe surface

For each MCP tool, identify the upstream Huly REST endpoint (or determine there isn't one):

| MCP tool | Probable REST endpoint | Verified? |
|---|---|---|
| `huly_find` | `GET /api/v1/find-all/{ws}?class=&query=` | |
| `huly_get` | `GET /api/v1/find-all/{ws}` (single result) | |
| `huly_create` | `POST /api/v1/tx/{ws}` (TxCreateDoc) | |
| `huly_update` | `POST /api/v1/tx/{ws}` (TxUpdateDoc) | |
| `huly_delete` | `POST /api/v1/tx/{ws}` (TxRemoveDoc) | |
| `huly_create_issue` | TxCreateDoc + TxCollectionCUD | |
| `huly_update_issue` | TxUpdateDoc | |
| `huly_find_issues` | findAll + lookup | |
| `huly_get_issue` | findOne + relations lookup | |
| `huly_find_cards` | findAll filtered by MasterTag | |
| `huly_create_card` | TxCreateDoc with MasterTag class | |
| `huly_sync_cards` | subprocess call (no transactor RPC) | n/a |
| `huly_upload_markup` | collaborator service, separate path | n/a |
| `huly_fetch_markup` | collaborator service | n/a |
| `huly_discover` | loadModel + findAll on multiple classes | |
| `huly_list_workspaces` | NATS announce (today) → `/api/v1/list-workspaces`? | TBD: this might not have a REST endpoint |
| `huly_status` | bridge-internal status (today) → drop? | TBD |

Reference: `huly.core/packages/api-client/src/rest/rest.ts`. The `RestClient` class there exercises every endpoint we need.

## Method

1. Worktree: `git worktree add ../huly-kube-p1-rest-spike spike/rest-from-mcp` (branch off `main`).
2. Add `RestHulyClient` to `crates/huly-bridge/src/huly/rest_client.rs`. Implement the `HulyClient` trait or a parallel one — whichever is simpler for the spike. Use `reqwest` with bearer-JWT auth.
3. Wire MCP via a config flag `[mcp] transport = "rest"` (default `"ws"` to preserve current behavior). When `"rest"`, MCP bypasses bridge HTTP and calls REST directly using the JWT it would otherwise pass to bridge.
4. **JWT acquisition for the spike:** hardcode a JWT in the spike config from `accounts.huly.black.solutions/login → selectWorkspace` flow run manually. Don't build the JWT broker yet — that's P3.
5. End-to-end probes:
   - `huly_find` against muhasebot (read).
   - `huly_create` → `huly_update` → `huly_delete` round-trip on a throwaway issue.
   - `huly_discover` (multi-class loadModel + findAll).
6. Stress probes:
   - 100 sequential reads, measure p50/p95 latency vs WS-path baseline.
   - 10 concurrent mutations, observe whether transactor session affinity matters.
7. Failure probes:
   - Hit a known-bad workspace (force a 401/403). Verify error path.
   - Hit a known-bad ID (force a 422). Verify the new `Status.params` flows through (PR #11 already merged).

## Exit criteria

**Pass (REST is sufficient):**
- All listed tools work with `transport = "rest"`.
- p95 latency ≤ 2× WS-path baseline on the 100-read stress.
- Concurrent mutations succeed without manual session/cookie handling.
- Failure modes surface useful error text.

**Fail (REST insufficient — fall back per-workspace WS in MCP):**
- Any tool needs an endpoint that isn't exposed via REST.
- p95 latency >2× WS baseline (chatty workflow penalty too high).
- Transactor demands sticky session that `reqwest` can't transparently maintain.

## Output artifacts

- `docs/research/p1-rest-spike-report.md` — verdict, evidence, p50/p95 numbers, list of REST endpoints used, any blockers found.
- Spike code lives in branch `spike/rest-from-mcp`. **Do not merge to main.** It informs P4's actual implementation; P4 will rewrite cleanly.

## Acceptance

- [ ] Spike report written with verdict (PASS/FAIL) and concrete recommendation.
- [ ] If PASS: list of REST endpoints to call from `huly-client::RestHulyClient` in P4.
- [ ] If FAIL: enumerate the missing endpoints + recommended WS approach for MCP.
- [ ] Spike branch pushed for reference; not merged.
