# P1 — REST spike report

**Branch:** `spike/rest-from-mcp`
**Date:** 2026-04-30
**Author:** dev
**Status:** spike — do not merge to main

## 1. Verdict

**PASS (REST sufficient) — pending live latency probe.**

Static analysis of `huly.core/packages/api-client/src/rest/{rest,tx}.ts` shows
every RPC the bridge currently issues over WS has a 1:1 REST endpoint on the
transactor, including multi-step `TxApplyIf` (which is itself a Tx and rides
through `POST /api/v1/tx/{ws}` like any other). No transactor RPC the MCP tool
surface needs is WS-exclusive.

Live RPC round-trip on `huly.black.solutions` was **not executed** — no JWT was
available in this environment and building a JWT broker is out of scope (P3).
The transactor host is reachable (`/config.json` returns 200, version 0.7.413
≥ the 0.7.19 floor where REST shipped), and 401/HTML-fallback responses on
unauthenticated probes match the expected proxy topology. A live latency
measurement is the **one remaining unknown**; if p95 ≥ 2× WS baseline the
verdict flips. Treat the verdict as PASS-conditional until P3 lands a token
and we run the 100-read stress.

## 2. Endpoint coverage

All path citations are `huly.core/packages/api-client/src/rest/rest.ts`
unless noted. `RestClientImpl` exercises every endpoint listed.

| MCP tool | Underlying call | REST endpoint | Source | REST-reachable? |
|---|---|---|---|---|
| `huly_find` | `findAll(class, query, opts)` | `GET /api/v1/find-all/{ws}?class=&query=&options=` | `rest.ts:97-157`, request URL line 110 | yes |
| `huly_get` | `findOne` (= `findAll` with `limit:1`) | `GET /api/v1/find-all/{ws}?...&options={"limit":1}` | `rest.ts:248-254` | yes |
| `huly_create` | `tx(TxCreateDoc)` | `POST /api/v1/tx/{ws}` | `rest.ts:256-277`, line 257 | yes |
| `huly_update` | `tx(TxUpdateDoc)` | `POST /api/v1/tx/{ws}` | `rest.ts:256-277` | yes |
| `huly_delete` | `tx(TxRemoveDoc)` | `POST /api/v1/tx/{ws}` | `rest.ts:256-277` | yes |
| `huly_create_issue` | `tx(TxApplyIf{matches, [TxUpdateDoc(inc), TxCollectionCUD(create)]})` plus optional collaborator upload + follow-up `tx(TxUpdateDoc)` for markup ref | `POST /api/v1/tx/{ws}` (single call); collaborator service is separate (`COLLABORATOR_URL`) | upstream `core/operations.ts:519` confirms `TxApplyIf extends Tx`; `tx.ts:107-109` shows it routes through `client.tx()` | yes |
| `huly_update_issue` | `tx(TxUpdateDoc)` (+ collaborator upload if description changed) | `POST /api/v1/tx/{ws}` + collaborator | `rest.ts:256-277` | yes |
| `huly_find_issues` | `findAll(tracker:class:Issue, query, {lookup})` | `GET /api/v1/find-all/{ws}` | `rest.ts:97-157`; lookup map handling at lines 125-139 (server returns `lookupMap`, client expands inline) | yes |
| `huly_get_issue` | `findOne(tracker:class:Issue, {identifier}, {lookup})` | `GET /api/v1/find-all/{ws}` (limit 1) | as above | yes |
| `huly_find_cards` | `findAll(card:class:Card or specific MasterTag ref, query)` | `GET /api/v1/find-all/{ws}` | `rest.ts:97-157` | yes |
| `huly_create_card` (via existing `huly_create`) | `tx(TxCreateDoc)` with MasterTag class | `POST /api/v1/tx/{ws}` | `rest.ts:256-277` | yes |
| `huly_create_component` | `findOne` (de-dupe) + `tx(TxCreateDoc)` | `GET /api/v1/find-all/{ws}` then `POST /api/v1/tx/{ws}` | as above | yes |
| `huly_link_issue_to_card` | `findOne(Association)` + `tx(TxCreateDoc{Relation})` | same pair as above | as above | yes |
| `huly_create_project` | `findOne` + `tx(TxCreateDoc{tracker:class:Project})` | same pair | as above | yes |
| `huly_discover` | parallel `findAll` across {Project, Component, IssueStatus, MasterTag, Association, Issue}; **no** `loadModel` needed at request time (the bridge already caches schema via `huly_common::schema`) | 6 × `GET /api/v1/find-all/{ws}` | `rest.ts:97-157` | yes |
| `huly_upload_markup` | collaborator service POST (separate domain) | `wss://.../_collaborator` per `/config.json` | not transactor — orthogonal to D1 | n/a (separate service, already REST-ish) |
| `huly_fetch_markup` | collaborator service GET | same | same | n/a |
| `huly_sync_status` | filesystem only | none | bridge-internal | n/a |
| `huly_sync_cards` | subprocess | none | bridge-internal | n/a |
| `huly_list_workspaces` | NATS announcement registry today; **alternative** = `accounts.huly...` `getUserWorkspaces` | `POST {ACCOUNTS_URL}` with `{method:"getUserWorkspaces", params:[]}` | `huly.core/packages/account-client/src/client.ts:349-355` | yes — but on the **account service**, not transactor; needs P3's account-token flow |
| `huly_status` | bridge-internal, will disappear in v2 | none | n/a | n/a |

Bridge-side helpers that are not user-facing tools but are exercised by the
existing `huly-bridge::HulyClient` trait:

| Bridge call | Maps to REST | Source |
|---|---|---|
| `add_collection` | `tx(TxCollectionCUD{create})` | `rest.ts:256-277` |
| `update_collection` | `tx(TxCollectionCUD{update})` | same |
| `apply_if_tx` | `tx(TxApplyIf{ matches, notMatches, txes })` | same; `TxApplyIf extends Tx` per `huly.core/packages/core/src/tx.ts:121` |
| `loadModel` (only used at bridge bootstrap today) | `GET /api/v1/load-model/{ws}?full=` | `rest.ts:218-246`; already implemented in `crates/huly-bridge/src/huly/rest.rs::get_model` |
| `getAccount` (bootstrap) | `GET /api/v1/account/{ws}` | `rest.ts:200-216`; already implemented in bridge |

## 3. Concerns

### 3.1 TX session affinity — non-issue
WS path keeps a session because the transactor multiplexes RPCs over a single
authenticated socket. REST is stateless: each call carries its bearer JWT and
workspace ref in the URL. `RestClientImpl` (TS reference) holds **only**
endpoint + workspace + token (`rest.ts:73-79`); no cookie jar, no per-session
state. Concurrent requests from one client are independent. Confirmed by the
Rust `RestClient` already in `crates/huly-bridge/src/huly/rest.rs` — uses
plain `reqwest::Client` with no middleware.

### 3.2 Multi-step ops — handled by `TxApplyIf`
The reference TS client wraps optimistic-concurrency mutations in
`TxApplyIf{matches, notMatches, txes}`, which is itself a `Tx` (`tx.ts:121`)
and ships in a single `POST /api/v1/tx/{ws}` body. The bridge today already
emits this shape (`crates/huly-bridge/src/huly/client.rs:403` `apply_if_tx`)
over WS — it just needs to be POSTed instead. No protocol change.

The one remaining multi-call workflow is `create_issue_with_markup_description`
(create issue, upload markup blob, update doc to point at the blob). That's
three calls today and stays three calls under REST; the second call hits the
collaborator service, not the transactor. No regression.

### 3.3 Schema resolution — already decoupled
`huly_discover` and per-class lookups already pull the full hierarchy from
`huly_common::schema` (built into the binary or fetched once at bridge boot).
We do **not** need to call `/api/v1/load-model` per MCP request. P2 hoist work
preserves this caching.

### 3.4 Auth / JWT subtleties
- Transactor accepts `Authorization: Bearer {token}` (`rest.ts:81-87`); same
  token format as the WS JWT, just delivered as a header.
- `/config.json` is **unauthenticated** and intentionally never sends the
  bearer (Rust impl already enforces this — `rest.rs:243-248`).
- 429 carries `Retry-After-ms` plus `X-RateLimit-*` headers (`rest.ts:181-198`).
  Already wired in Rust (`rest.rs:381-384`, `RateLimitInfo::from_headers`).
- 4xx error bodies sometimes carry `{ error: Status }` JSON, sometimes plain
  text; `RestClientImpl::tx` checks both shapes (`rest.ts:259-275`). Our Rust
  `RestError::Upstream` currently surfaces only the raw body — P4 should add
  a `Status` decode pass before returning, otherwise we lose `Status.params`
  fidelity that PR #11 just added on the WS path.
- No CSRF / no cookies. Bearer header is the entire auth surface.

### 3.5 Body compression
Transactor advertises `accept-encoding: snappy, gzip` (`rest.ts:81-87`).
Our `RestClient::send` already decodes snappy
(`rest.rs:399-411`). reqwest handles gzip transparently when the feature is
on — verify in `Cargo.toml` for P4. Not blocking the spike.

### 3.6 Workspace-id semantics
REST URLs embed the workspace **uuid/id** (the same id the WS hello carries);
not the human-readable `huly.black.solutions/muhasebot` string. The
account-service `selectWorkspace` flow returns both `endpoint` and
`workspaceId` (`rest.ts:54-57` `connectRest`). P3 broker must surface both;
caching by workspace-uuid (not human slug) is the right key.

## 4. Latency assessment

**Not measured.** No JWT in this environment, transactor live calls require
the account-flow `selectWorkspace` exchange.

Expected differences vs WS baseline, reasoned:

- **HTTPS handshake.** First call per workspace pays ~1 round-trip for TCP +
  ~2 for TLS (≈ 3× RTT to the transactor). `reqwest::Client` keeps a connection
  pool, so subsequent calls reuse the connection (single-RTT overhead per
  request, plus TLS record framing).
- **Per-request framing.** REST has HTTP request/response headers (~hundreds
  of bytes) on top of the JSON body; WS just frames the JSON. For sub-KB
  payloads this is meaningful (~10-30% per-call overhead); for typical issue
  bodies (1-10 KB) it's noise.
- **Multi-call workflows.** `huly_create_issue` is 1 transactor call (the
  bundled `TxApplyIf`) plus 1 optional collaborator call. Same shape as WS.
  Not a chatty workflow.
- **No batching penalty for `huly_discover`.** Six `findAll` calls in parallel
  reuse the connection pool. Expected total ≤ slowest single call + small
  fan-out cost. WS multiplexes on one socket and does similar.

**Best estimate:** p50 +1-3 ms vs WS for already-warmed connections, p95
within 1.5× for cold-pool cases (first request per workspace per process).
The brief's 2× p95 ceiling should be comfortable. **Confirm with live probe
before P4 commits.**

## 5. Recommended P4 client surface

Method list for `huly-client::RestHulyClient` (the trait already exists in the
bridge as `HulyClient`; lifting it to `huly-client` keeps the names). All
methods take `workspace: &str` and assume `RestClient` was constructed with the
transactor endpoint + bearer JWT.

```rust
pub trait RestHulyClient: Send + Sync {
    // Reads
    async fn find_all(
        &self, ws: &str,
        class: &str, query: Value, options: Option<FindOptions>,
    ) -> Result<FindResult, RestError>;

    async fn find_one(
        &self, ws: &str,
        class: &str, query: Value, options: Option<FindOptions>,
    ) -> Result<Option<Doc>, RestError>;

    async fn search_fulltext(  // already implemented
        &self, ws: &str, query: &str, opts: &SearchOptions,
    ) -> Result<SearchResult, RestError>;

    // Mutations — all single endpoint, just different Tx body
    async fn tx(&self, ws: &str, tx: Value) -> Result<Value, RestError>;

    // Convenience wrappers (build the Tx body, call tx())
    async fn create_doc(&self, ws: &str, class: &str, space: &str, attrs: Value)
        -> Result<String /* id */, RestError>;
    async fn update_doc(&self, ws: &str, class: &str, space: &str,
        id: &str, ops: Value) -> Result<Value, RestError>;
    async fn remove_doc(&self, ws: &str, class: &str, space: &str, id: &str)
        -> Result<Value, RestError>;
    async fn add_collection(&self, ws: &str, /* TxCollectionCUD args */)
        -> Result<String, RestError>;
    async fn update_collection(&self, ws: &str, /* ... */)
        -> Result<Value, RestError>;
    async fn apply_if_tx(&self, ws: &str, scope: &str,
        matches: Vec<Value>, not_matches: Vec<Value>, txes: Vec<Value>,
    ) -> Result<ApplyIfResult, RestError>;

    // Bootstrap
    async fn get_account(&self, ws: &str) -> Result<Account, RestError>;
    async fn get_model(&self, ws: &str, full: bool) -> Result<Vec<Tx>, RestError>;
}
```

Notes:

- Most of the convenience layer is already in
  `crates/huly-bridge/src/huly/txcud.rs` (used by MCP `tools.rs`); P4 lifts it
  to `huly-client` unchanged.
- `RestClient` (the HTTP-level type) is already implemented in
  `crates/huly-bridge/src/huly/rest.rs`. Missing methods: `find_all` and `tx`.
  Adding them is mechanical (~80 LOC + tests).
- Collaborator + accounts services are **separate clients**; do not stuff
  them into `RestHulyClient`. They live as `CollaboratorClient` and
  `AccountClient` (the latter is P3's job).

## 6. Open risks

1. **Latency unmeasured.** Verdict flips to FAIL if live p95 ≥ 2× WS.
   Mitigation: P3 lands a JWT, then run the 100-read stress before P4 commits.
2. **`Status.params` decode on REST 4xx.** The current Rust `RestError::Upstream`
   surfaces raw body. PR #11 (`Status.params` capture on WS path) will silently
   regress under REST unless P4 adds the same decode. Two-line fix in
   `RestClient::send`; track in P4.
3. **Workspace identifier mismatch.** REST URLs use workspace uuid; bridge
   announcements today use the human host/slug. P3 broker must return both;
   MCP must key its REST client cache on uuid.
4. **Connection pool exhaustion across many workspaces.** One process holding
   `reqwest::Client`s for N workspaces × M concurrent users could stress the
   pool. `reqwest::Client` is fine (it's pool-per-host), but P6 (per-workspace
   processes) makes this moot anyway.
5. **Rate limit headers vs WS.** WS path does not surface 429 today; REST
   surfaces a structured `RateLimited` error with `Retry-After-ms`. MCP tool
   handlers must translate this to a useful tool error message (e.g. "Huly
   rate-limited the request — retry in N ms"); fold into P0's iserror
   mapping.
6. **`/config.json` workspace overrides.** If multi-region deployment ever
   ships a per-workspace transactor `endpoint` (today they all share one),
   the broker is the only place that knows the right URL — MCP must not
   hardcode it. Already accommodated by P3 design.
7. **`huly_list_workspaces` REST path** runs against the **account service**,
   not the transactor, and uses a different token (account JWT, not workspace
   JWT). P3 must distinguish the two. Today bridge gets this for free via NATS
   announcements; the v2 design should keep the account-service path optional
   and prefer NATS until P6 removes the bridges entirely.
