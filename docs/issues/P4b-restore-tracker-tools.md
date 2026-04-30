# P4b — Restore tracker/markup/sync tools under new architecture

**Phase:** P4b (follow-up to P4 — was deferred during the big consolidation).
**Branch:** `feat/restore-tracker-tools`
**Worktree:** `../huly-kube-p4b-restore`
**Depends on:** P4 (merged).
**Blocks:** P7 (audit publishes touch the same tool methods).

## Why this exists

P4 deleted `crates/huly-mcp/src/mcp/tools.rs` and `schema_cache.rs` along with the bridge HTTP gateway. The MCP tools that depended on those modules went with them. Result: the remaining MCP tool surface is only primitive CRUD (`huly_find`, `huly_get`, `huly_create`, `huly_update`, `huly_delete`, `huly_list_workspaces`). The high-level helpers — which are what the muhasebot cleanup blocker actually needs — are gone.

This PR restores them, reimplemented against `huly_client::HulyClient` (via `huly_client_factory`) and the existing `huly_client::collaborator` (for markup tools).

## Tools to restore

Reimplement against the new architecture (no bridge HTTP, JWT-broker driven, schema cache via factory):

| Tool | Why | Notes |
|---|---|---|
| `huly_create_issue` | tracker mutation | TX wrapper: TxCreateDoc + TxCollectionCUD for tracker:class:Issue. Use schema cache for class id resolution. |
| `huly_update_issue` | tracker mutation | TxUpdateDoc with operation set. |
| `huly_find_issues` | tracker query | findAll on tracker:class:Issue with filters. |
| `huly_get_issue` | tracker query + relations | findOne + lookup chain (component, parent issue, etc.). |
| `huly_create_component` | tracker mutation | TxCreateDoc for tracker:class:Component. |
| `huly_create_project` | tracker mutation | TxCreateDoc for tracker:class:Project + TxCreateDoc for ProjectIdentifier. |
| `huly_find_cards` | card query | findAll filtered by MasterTag. Names→ids via schema cache. |
| `huly_create_card` | card mutation | TxCreateDoc with MasterTag class. |
| `huly_link_issue_to_card` | relation create | TxCreateDoc for the Association/Relation type. |
| `huly_discover` | workspace introspection | loadModel + findAll on Tracker projects, components, MasterTags, Associations, status categories. |
| `huly_upload_markup` | collaborator service | use `huly_client::collaborator` directly. JWT from broker. |
| `huly_fetch_markup` | collaborator service | symmetric to upload. |
| `huly_sync_cards` | subprocess wrapper | restore as-is — it shells out to a sync binary; no architecture concern. |

## Reference for previous implementations

Before P4 deleted them, these tools lived in `crates/huly-mcp/src/mcp/tools.rs` (now gone). To recover the algorithmic details: `git show <pre-P4-commit>:crates/huly-mcp/src/mcp/tools.rs`. The merge commit before deletion is the parent of `feat/mcp-direct-and-bridge-cleanup`'s deletion commit (`61a750a`); use `git log --diff-filter=D --name-only feat/mcp-direct-and-bridge-cleanup -- 'crates/huly-mcp/src/mcp/tools.rs'` to find it.

## Schema-event invalidation (D9)

P4 wired only TTL refresh. This PR adds the **subscribe-first** invalidation path:
- New module `crates/huly-mcp/src/schema_invalidator.rs` (or fold into `huly_client_factory.rs`).
- Subscribes to `huly.event.tx.>` (filtered to class/attribute mutations only via filter or per-message inspect).
- On a matching TX for workspace W, invalidates schema cache entry for W; next tool call refetches.
- Fall through to TTL if subscriber dies (per D9: "Fallback: TTL of 5 minutes" already in P4).
- One unit test that publishes a synthetic class-mutation TX and asserts cache invalidation.

## Account-service URL fix-up

Per P4's report: `huly_list_workspaces` currently uses a stubbed `derive_accounts_url` returning `huly.black.solutions`. Fix:
- `MintResponse` already carries `rest_base_url`. The accounts URL is derivable: `{rest_base_url with /api/v1 stripped}/accounts` (or pull `accounts_url` from existing bridge config and ship it through the broker — likely cleaner).
- Decision: extend `MintResponse` with `accounts_url` (small P3 retroactive change, in this PR).
- MCP factory caches it alongside the JWT.
- `huly_list_workspaces` reads it from the cached factory entry.

## Constraints

- Conventional commits, GPG-signed, no `Co-Authored-By` trailer, no push, no PR.
- Each commit builds + tests green.
- Use **the worktree path** for all edits — verify with `pwd` before each Edit/Write batch. Do not write under `/mnt/workspace/server-yazilim/huly-kube/...` directly.

## Acceptance

- [ ] All 13 tools reimplemented and tested.
- [ ] `make clippy` clean.
- [ ] `make test` green; new tests for each tool's happy path + at least one error path.
- [ ] Schema invalidation via NATS event subscriber works (unit test).
- [ ] `huly_list_workspaces` derives accounts URL correctly (no stub).
- [ ] No reference to deleted bridge HTTP paths in any tool implementation.
- [ ] `huly-mcp` test count returns to ≥110 (pre-P4 baseline).

## Out of scope

- P7's audit publishes (`huly.mcp.tool.invoked`, etc.) — that's the next phase.
- Per-tool rate-limit overrides (D6 default 3 retries is fine).
- Markdown ↔ ProseMirror conversion changes.

## Suggested commit splits

1. `feat(huly-mcp): restore primitive tracker tools (find_issues, get_issue, update_issue)`
2. `feat(huly-mcp): restore tracker mutation tools (create_issue, create_component, create_project)`
3. `feat(huly-mcp): restore card tools (find_cards, create_card, link_issue_to_card)`
4. `feat(huly-mcp): restore discovery tool`
5. `feat(huly-mcp): restore markup + sync tools`
6. `feat(huly-mcp): schema cache invalidation via huly.event.tx subscriber`
7. `feat(huly-common,mcp): plumb accounts_url through MintResponse`

## Return

A report of comparable detail to P4's. Note any tools where the original implementation no longer applies under the new architecture and why your reimplementation diverges.
