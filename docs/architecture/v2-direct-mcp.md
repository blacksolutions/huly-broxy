# v2 — Direct-MCP architecture

Status: **ratified** (open questions resolved). Owner: Murat. Last updated: 2026-04-30.

> Beta-phase project. **No backward-compat shims** between phases. Mixed-deployment migration windows are not required (open question 5).

## TL;DR

- **huly-mcp** talks directly to the Huly transactor per workspace. No bridge in its request path.
- **huly-bridge** is the governor: holds one WS per workspace, mirrors transactor pushes to NATS, mints workspace JWTs on demand. **Not a request proxy.**
- **NATS** is the public event bus for 3rd-party integrations, not internal RPC plumbing.
- Both processes publish to NATS in disjoint subject namespaces (`huly.event.*` from bridge, `huly.mcp.*` from mcp), correlated by `request_id`.
- **Delete** `crates/huly-bridge/src/admin/` (the HTTP gateway), `crates/huly-mcp/src/bridge_client.rs`, and the `huly.bridge.{announce,lookup,schema}` discovery subjects.

## Why

Today's drift from the original design intent:

| Intent | Today | Consequence |
|---|---|---|
| MCP → Huly directly | MCP → bridge HTTP `/api/v1/*` → Huly WS | Bridge is on the request path. MCP-driven failures (panic, OOM, deadlock in pending-request map) take down event forwarding for *all* workspaces. The "governor isolates failures" property is not actually held. |
| NATS = event bus for 3rd parties | NATS = announce/lookup RPC for MCP-to-bridge discovery | Subjects intended for external consumers (`huly.event.*` populated by `bridge/event_loop.rs`) **have zero readers**. The actual NATS traffic is internal plumbing. |
| Bridge per workspace | One bridge process, multiple workspaces multiplexed via routing | Crash blast radius = all workspaces. |

Recent fixes (#9, #10, #11) addressed *symptoms*: wildcard announces, opaque transactor errors, MCP swallowing errors. The architecture itself is what produced those classes of bug:

- The wildcard-host footgun only exists *because* MCP needs to dial bridge HTTP. Direct MCP doesn't.
- The MCP-to-bridge HTTP layer has its own auth (`bridge_api_token`), error-mapping (`platform_api::ApiError`), validation, and rate limiting — duplicated effort. The transactor already validates everything.
- The error-text loss bug (#11) traversed two layers (transactor → bridge `RpcError` → bridge HTTP → MCP `bridge_client` → tool string), each shedding context. Direct MCP has one layer.

## Target shape

```
                                                 Huly transactor (per workspace)
                                                  ▲                  ▲
                                                  │ WS               │ WS or REST
                                                  │ events           │ CRUD
                                                  │                  │
   3rd-party                              ┌───────┴──────┐   ┌───────┴────────┐
   integrations  ────────────► NATS ◄──── │ huly-bridge  │   │ huly-mcp       │
                  huly.event.*            │  (governor)  │   │  (direct CRUD) │
                  huly.mcp.*              │              │   │                │
                                          │ JWT mint ────┼──►│ requests JWT   │
                                          └──────────────┘   └────────────────┘
                                                              ▲
                                                              │ stdio
                                                              │
                                                          Claude Code
                                                          / other MCP host
```

Three independent processes, single shared library:

- `huly-client` (new crate, hoisted from `crates/huly-bridge/src/huly/`) — transactor protocol (WS+RPC, accounts/REST, types). Both processes depend on it.
- `huly-bridge` — governor. Owns WS per workspace, forwards transactor pushes to NATS, exposes a small JWT-mint endpoint.
- `huly-mcp` — direct client. Per-workspace transactor connection (or stateless REST per call — TBD by spike), publishes audit events to NATS.

## Decisions (need ratification)

### D1 — Transport for MCP → transactor: **REST first, WS only if needed**

The transactor exposes both. REST is simpler (stateless, no reconnect, no pending map, no ping). The Huly upstream `api-client` provides REST shapes for `findAll`, `tx`, `loadModel`, `searchFulltext`. MCP only needs CRUD-ish operations; it does not consume server pushes. **Default to REST.** Reserve WS for if the spike (P1, below) finds a missing endpoint.

Risks: TX mutations may have transactor session affinity (cookie / sticky session) that REST handles transparently but worth verifying. Latency: per-call HTTPS handshake overhead, mitigated by `reqwest` connection pool.

### D2 — JWT source for MCP: **bridge as credential broker (option c)**

MCP does not store passwords. On startup (or per-workspace on first use), MCP requests a workspace JWT from the bridge over a small NATS req/reply subject `huly.bridge.mint`. Bridge does the password→login→`selectWorkspace` flow on its trusted host, returns the JWT (and refresh policy). MCP caches the JWT in process memory only, refreshes before expiry.

```
mcp                     bridge                accounts.huly
 │                         │                       │
 │── nats req: mint ───────►                       │
 │   {workspace, agent_id} │── login + selectWS ───►
 │                         │◄──── jwt ─────────────│
 │◄────── jwt ─────────────│
 │                         │
 │── HTTPS Authorization: Bearer jwt ─────────────►transactor
```

Benefits over (a) static config in mcp.toml: credentials never leave the trusted bridge host. Benefits over (b) operator-driven login: zero friction; agents work out-of-the-box.

Cost: bridge becomes load-bearing for MCP *startup* (and JWT refresh), but **not for ongoing requests**. Bridge crash → existing JWTs continue to work until expiry → MCP keeps working. Crash blast radius is bounded.

### D3 — NATS subject taxonomy

| Subject | Publisher | Consumers | Payload |
|---|---|---|---|
| `huly.event.tx.{class}.{op}` | bridge | 3rd parties, audit | Transactor TX (canonical, ordered) |
| `huly.event.workspace.{ready,disconnected,degraded}` | bridge | 3rd parties | Workspace lifecycle from bridge POV |
| `huly.mcp.tool.invoked` | mcp | audit | `{tool, workspace, agent_id, params_digest, request_id}` |
| `huly.mcp.tool.completed` | mcp | audit | `{request_id, ok|err, duration_ms, result_digest|error}` |
| `huly.mcp.action.{class}.{op}` | mcp | audit | Concretized intent: `huly.mcp.action.tracker.issue.delete` `{workspace, id, request_id}` |
| `huly.mcp.error` | mcp | audit, ops | Tool-level failure with full transactor `params` |
| `huly.bridge.mint` (req/reply) | mcp → bridge | bridge | JWT mint request |

Removed from current code:
- `huly.bridge.announce`
- `huly.bridge.lookup` (req/reply)
- `huly.bridge.schema.{workspace}` (req/reply) — schema fetched from transactor directly via REST `loadModel`

**`request_id` is load-bearing.** Generated at MCP, plumbed into the transactor TX via `meta.request_id`, surfaced in both `huly.event.tx.*` and `huly.mcp.*` payloads. Lets a subscriber join "what the AI requested" with "what the transactor actually did".

### D4 — Bridge process model: **one process per workspace, supervised**

Today: one bridge serves N workspaces in one process. A panic in workspace A's WS handler can crash B's. Move to one bridge process per workspace, supervised by systemd template units (`huly-bridge@<workspace>.service`). Crash isolation by OS, no shared state.

Per-workspace config: TOML drop-in directory (`/etc/huly-bridge/workspaces.d/<workspace>.toml`), or one TOML with a `[[workspace]]` array consumed by a parent supervisor that spawns instances. Prefer the systemd-template path — it composes with existing systemd patterns and per-workspace journald scope is free.

Cost: one extra resident process per workspace. Each is ~20MB of memory. Negligible on Riven.

### D5 — Schema resolution: **MCP fetches via REST, no NATS round-trip**

Today: bridge resolves schema once, MCP fetches via `huly.bridge.schema.{ws}` req/reply. Replace with: MCP calls `loadModel` over REST against the transactor directly, caches in process memory keyed by hash of `(workspace, modelHash from accounts response)`. One HTTPS call per cold start per workspace.

Benefit: MCP and bridge schema views can never disagree (same source). No subject to maintain.

## Out of scope (explicit)

- **Windows runtime.** QA owns sign-off (per CLAUDE.md). v2 development happens on Linux.
- **MCP horizontal scaling.** One MCP process per agent host is the model. Multi-tenant MCP is a separate concern.
- **Bridge HA.** Single bridge per workspace is acceptable. Add HA when product needs justify it.

## Refactor sequence

Each step is one PR. **Do not bundle.**

### P0 — Land deferred fixes (~now)

Independent of v2, but unblocks diagnosis and reduces churn during the refactor.

- [ ] **Bug #2 — MCP isError mapping.** Tool methods return `Result<String, String>` instead of swallowing errors into `String`. ~20 methods. Mechanical.
- [x] **#11** — bridge captures `Status.params` (already merged).
- [ ] **muhasebot diagnosis.** Redeploy bridge → trigger one mutation → read the new log line → name the actual failure mode. Drives whether v2 inherits any unknown blocker.

### P1 — Spike: direct REST from MCP

Worktree: `git worktree add ../huly-kube-rest-spike`. ~2–3 days.

Scope:
- Add a `RestHulyClient` to `crates/huly-bridge/src/huly/` (still in bridge crate at this stage).
- Implement `find_all`, `remove_doc`, `update_doc`, `create_doc`, `add_collection`, `update_collection`, `remove_collection`, `load_model` over REST.
- Wire MCP to use it via a config flag `mcp.transport = "rest"`.
- End-to-end test: one `huly_delete` against muhasebot, then back-to-back N×100 reads/writes for stability.

Exit criteria:
- All MCP tools work with `transport = "rest"`.
- p95 latency within 2× of WS path on a chatty workload.
- No transactor-side session affinity issue surfaced.

If exit criteria fail → fall back to WS for MCP, design doc updated, JWT broker still happens but MCP keeps a per-workspace WS.

### P2 — Hoist `huly-client` crate

- New crate `crates/huly-client`. Move `huly/{accounts,client,connection,proxy,rpc,schema_resolver,types,workspace_*}.rs`.
- `huly-bridge` and `huly-mcp` depend on it.
- One mechanical PR. No behavior change.

### P3 — JWT broker (D2)

- Bridge subscribes to `huly.bridge.mint` (NATS req/reply).
- Returns `{jwt, expires_at, refresh_at}`.
- MCP fetches on cold start + before expiry.
- Tests: mint, expiry, refresh path.

### P4 — MCP direct CRUD + bridge cleanup (combined per D10)

Lockstep: this PR replaces MCP's bridge dependency *and* deletes the now-unreachable bridge code. No mixed-deployment window.

- MCP uses `huly-client::RestHulyClient` (or `WsHulyClient` per P1 outcome).
- Delete `crates/huly-mcp/src/bridge_client.rs`.
- Delete `crates/huly-mcp/src/discovery.rs`.
- Delete `crates/huly-bridge/src/admin/` (HTTP gateway, ~1500 LOC).
- Delete `crates/huly-bridge/src/bridge/announcer.rs` (announce + lookup).
- Delete `crates/huly-bridge/src/bridge/schema_resolver.rs`.
- Delete from `huly-common::announcement`: `BridgeAnnouncement`, `LOOKUP_SUBJECT`, `SCHEMA_FETCH_SUBJECT_PREFIX`, `WorkspaceSchemaResponse`.
- Bridge becomes ~30–40% of current size.

### P6 — One-process-per-workspace (D4)

- systemd template `huly-bridge@.service`.
- Per-workspace TOML in `/etc/huly-bridge/workspaces.d/`.
- Ansible templates for the new layout.
- Migration runbook for the muhasebot host.

### P7 — `huly.mcp.*` audit publishes (D3)

- MCP gets a NATS publisher.
- Tool methods emit `huly.mcp.tool.{invoked,completed}` and `huly.mcp.action.*`.
- `request_id` plumbed through TX `meta.request_id`.
- Document subject schema.

## Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| REST endpoint missing for some operation MCP uses | medium | P1 spike covers full surface; fall back to WS per workspace |
| TX session affinity required | low | Verify in P1; sticky sessions via cookies if needed |
| JWT mint becomes a bottleneck | low | One mint per workspace per MCP cold start; refresh is cheap |
| Per-workspace bridge process count grows beyond Riven's resources | low | ~20MB each, current count <10 workspaces |
| 3rd-party integrations not yet built — `huly.event.*` consumers absent | high | Document subjects now; build at least one example consumer in `examples/` |
| Refactor takes longer than expected, blocking other work | medium | Each PR is independent and reverts cleanly; can pause between any two |

## Resolved decisions (was: open questions)

### D6 — Rate limiting: **MCP honors transactor rate limits, no gateway**

Rate limiting belongs on the transactor side (already enforced there). MCP must respect the transactor's `429 Too Many Requests` + `Retry-After` header with exponential backoff, and proactively read any rate-limit hints the transactor announces (`X-RateLimit-Remaining`, etc.). MCP must not silently retry past `Retry-After` — failures surface as `huly.mcp.error` so external rate-limit dashboards can react.

No bridge-side rate limit; the bridge HTTP gateway is being deleted, and the bridge's WS path doesn't proxy MCP traffic in v2.

Implementation contract: MCP's HTTP client wraps `reqwest` with a token-bucket per workspace, primed from any rate-limit hints in transactor responses. Concurrent in-flight per workspace also capped (default 8) to prevent a runaway agent from saturating the upstream.

### D7 — Audit retention: **default NATS pub/sub, no JetStream**

Consumers persist what they care about. No JetStream stream for `huly.mcp.>` at v2 launch. Revisit when a compliance requirement justifies the operational cost.

### D8 — Agent identity: **mandatory, no auto-fallback**

`agent_id` is required in every MCP startup. **No hostname+pid fallback** — anonymized identifiers defeat the audit purpose. Sourced in priority order:

1. `HULY_MCP_AGENT_ID` env var (explicit, recommended for production agents).
2. MCP `initialize` request `clientInfo.name + clientInfo.version` (rmcp 1.3 surfaces this — Claude Code passes its identity here).
3. **Fail to start** if neither is present. Loud error: "agent_id is required; set HULY_MCP_AGENT_ID or upgrade your MCP host to one that sends clientInfo".

Stamped into `huly.mcp.tool.invoked` payload and into transactor TX `meta.request_id` correlations.

### D9 — Schema cache invalidation: **subscribe-first, TTL-fallback**

Primary: MCP subscribes to `huly.event.tx.*` filtered for `core:class:Class`, `core:class:Attribute`, `core:class:Mixin` operations. Any such TX invalidates the cached `WorkspaceSchema` for that workspace; MCP refetches `loadModel` on the next tool call.

Fallback: TTL of 5 minutes. Trips when (a) bridge subscription is unavailable, (b) bridge has dropped the workspace WS, (c) NATS connectivity is flaky. The 5-minute window bounds blast radius if the subscribe path silently breaks.

If a `huly.mcp.action.*` call hits a `Hierarchy.findClass` or similar schema-mismatch error, MCP **forces a refetch** before the next call (third recovery layer). Self-healing, no operator intervention.

### D10 — No mixed-deployment migration: **P4 → P5 in lockstep**

Beta phase. P4 (MCP direct) and P5 (bridge cleanup) merge in the same release window. No "leave gateway code for 2 weeks" period. Telemetry-wait was the cost of compatibility; we're not paying it.

Practical effect: one deploy ships both new MCP binary and reduced bridge binary. Coordinated restart (Ansible playbook orders bridge first, then MCP) is acceptable downtime for beta.

## Definition of done

- [ ] All P0–P7 PRs merged.
- [ ] muhasebot cleanup completes via direct MCP path with full audit trail in `huly.mcp.*`.
- [ ] At least one example 3rd-party integration in `examples/` consuming `huly.event.tx.*`.
- [ ] `crates/huly-bridge` LOC reduced ≥30%.
- [ ] `huly.bridge.{announce,lookup,schema}` subjects produce no traffic (verify via NATS monitoring) for 2 weeks; remove the subject constants.
- [ ] DEPLOY.md updated for systemd template units.
- [ ] One per-workspace bridge process running per workspace on Riven.

## Appendix — current code that goes away

Directly deletable once P5 lands:

- `crates/huly-bridge/src/admin/` (HTTP gateway, ~1500 LOC)
- `crates/huly-bridge/src/bridge/announcer.rs` (announce + lookup responder)
- `crates/huly-bridge/src/bridge/schema_resolver.rs` (if MCP fetches schema directly)
- `crates/huly-mcp/src/bridge_client.rs` (HTTP client to bridge)
- `crates/huly-mcp/src/discovery.rs` (NATS announce subscriber + registry + reaper)
- `crates/huly-common/src/announcement.rs` `BridgeAnnouncement`, `LOOKUP_SUBJECT`, `SCHEMA_FETCH_SUBJECT_PREFIX`, `WorkspaceSchemaResponse`
- Configs: `mcp.bridge_api_token`, `admin.api_token`, `admin.advertise_url` (the wildcard-host footgun also vanishes)

That deletion list is the v2 goal made concrete: anything in it that does not get deleted by P7 indicates the refactor is incomplete.
