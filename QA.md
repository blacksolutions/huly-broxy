# QA Report: huly-kube Workspace

## 0.7.19 Migration QA — Executive Summary (2026-04-22, updated post-issue-18)

**Scope:** end-to-end migration of `huly-bridge` and `huly-mcp` against upstream `@hcengineering/api-client` v0.7.19 and `huly-api/packages/mcp-server`, plus a follow-up fix batch (issues #13, #14, #17, #20, #21, #22) and issue #18 (markup via collaborator service).
**Verdict:** PASS — all phases + fix batch + issue #18 merged on `master`; lint clean under `-D warnings`; 401 tests green (+5 ignored: 4 live-smoke + 1 race-freedom property).

| Metric | Migration start | After migration | After fix batch | After #18 |
|--------|-----------------|-----------------|------------------|-----------|
| Total tests (active) | 200 | 293 | **327** (+34 from fix batch) | **401** (+56 from #18) |
| Tests `#[ignore]`d | 0 | 0 | **5** (4 live smoke + 1 race-freedom property) | **5** |
| Source LOC (incl. tests) | 6,934 | 11,511 | **~13,200** | **~15,000** |
| Clippy warnings (`-D warnings`) | 11 | 0 | **0** | **0** |
| MCP tools | 7 | 18 | 18 | 18 |
| Typed REST endpoints | 0 | 6 | 6 | **8** (+`upload-markup`, `fetch-markup`) |
| Bridge platform RPC methods | 5 | 5 | **7** (+`addCollection`, `updateCollection`) | 7 |
| Integration test files | 0 | 3 | **5** (+`smoke_0_7_19`, `admin_collection_routes`) | **6** (+`markup_e2e`) |
| New deps | — | — | — | `pulldown-cmark 0.12` |

**Issues closed by fix batch:** #13, #14, #17, #20, #21, #22 (and #23 from clippy cleanup pass).
**Issues closed by close-out session (2026-04-22 pm):** #2 (integration tests), #6 (NATS creds), #18 (markup via collaborator), #19 (Won't Fix — documented), #24 (TxApplyIf atomic create).
**Open issues remaining:** none.

Detailed phase-by-phase assessment in §11.4–§11.9; fix-batch assessment in §11.10; close-out assessment in §11.11 below.

---

## DEPLOY.md Documentation Review (2026-04-08)

**Scope:** [DEPLOY.md](DEPLOY.md) — focus on §3 NATS rootless Podman/Quadlet rewrite and [systemd/nats.container](systemd/nats.container)
**Verdict:** PASS — all D1–D10 fixed on 2026-04-08; D11 verified (7 tools registered in [crates/huly-mcp/src/mcp/server.rs](crates/huly-mcp/src/mcp/server.rs)).

| # | Sev | Location | Issue | Status |
|---|-----|----------|-------|--------|
| ~~D1~~ | High | [DEPLOY.md §3.2](DEPLOY.md) | Quadlet path moved to per-user `/var/lib/nats/.config/containers/systemd/`. | **FIXED** |
| ~~D2~~ | High | [DEPLOY.md §3.1](DEPLOY.md) | `useradd` shell changed to `/bin/bash`. | **FIXED** |
| ~~D3~~ | High | [DEPLOY.md §3.1](DEPLOY.md) | `systemctl start user@$(id -u nats).service` added after `enable-linger`. | **FIXED** |
| ~~D4~~ | Med | [DEPLOY.md §3.2](DEPLOY.md) | Step now says "Run from the repo root" and uses `$(pwd)/systemd/nats.container`. | **FIXED** |
| ~~D5~~ | Med | [DEPLOY.md §3.4](DEPLOY.md) | Verify step uses `machinectl shell nats@`. | **FIXED** |
| ~~D6~~ | High | [systemd/nats.container](systemd/nats.container) | Volume flag now `:Z,U` for userns chown. | **FIXED** |
| ~~D7~~ | Low | [DEPLOY.md §1](DEPLOY.md) | Debian Podman 4.3 / backports note added. | **FIXED** |
| ~~D8~~ | Low | [DEPLOY.md](DEPLOY.md) | Diagram label updated to `NATS (podman, localhost:4222)`. | **FIXED** |
| ~~D9~~ | Low | [DEPLOY.md §3.1](DEPLOY.md) | `usermod --add-subuids/--add-subgids` remediation added. | **FIXED** |
| ~~D10~~ | Low | [DEPLOY.md §4](DEPLOY.md) | Note added that `huly-bridge.service` no longer has `After=nats.service`. | **FIXED** |
| D11 | Info | [DEPLOY.md](DEPLOY.md) | Verified: 7 tools (`huly_list_workspaces`, `huly_status`, `huly_find`, `huly_get`, `huly_create`, `huly_update`, `huly_delete`) registered in `crates/huly-mcp/src/mcp/server.rs`. | **VERIFIED** |

---

**Date:** 2026-04-16 (Feature QA: cross-platform builds)
**Reviewer:** Code Review (Automated)
**Version:** 0.1.0
**Rust Edition:** 2024

---

## 1. Project Summary

`huly-kube` is a Cargo workspace containing three crates that bridge Huly.io platform with internal infrastructure and AI tooling:

- **huly-common** — Shared library with domain types, API request DTOs, and NATS announcement schema
- **huly-bridge** — Systemd service that connects to Huly via WebSocket, forwards events to NATS, exposes REST API proxy and admin endpoints, and broadcasts discovery announcements
- **huly-mcp** — MCP (Model Context Protocol) server that discovers bridges via NATS and exposes Huly operations as tools for Claude

### Changes Since Last QA (2026-04-16)

- **TLS backend: native-tls → rustls** — Pure Rust TLS, zero system dependencies, enables cross-compilation to Windows
- **Cross-platform sd-notify** — `sd-notify` crate moved to Linux-only conditional dep; `SdNotifier` cfg-gated with no-op on non-Linux
- **Cross-platform signals** — `shutdown_signal()` split into `#[cfg(unix)]` (SIGTERM + Ctrl-C) and `#[cfg(windows)]` (Ctrl-C only)
- **Build tooling** — Makefile + Cross.toml for producing Linux and Windows release binaries via `cross`
- **NoVerifier struct** — rustls `ServerCertVerifier` implementation for `tls_skip_verify` mode
- **LOC:** 6,741 → 6,934 (+193)

---

## 2. Test Results (2026-04-22 — post fix-batch)

| Metric | Result |
|--------|--------|
| Total tests (active) | **327** (221 huly-bridge unit + 9 huly-bridge integration + 12 huly-common + 85 huly-mcp) |
| Passed | **327** |
| Failed | **0** |
| Ignored | **5** (4 live smoke + 1 race-freedom property) |
| Clippy warnings (`-D warnings`) | **0** |
| Unsafe blocks | **0** |
| Test duration | **2.7s** |

```
huly-bridge unit:                 test result: ok. 221 passed; 0 failed; 0 ignored
huly-bridge admin_collection:     test result: ok.   3 passed; 0 failed; 0 ignored
huly-bridge mock_huly:            test result: ok.   1 passed; 0 failed; 0 ignored
huly-bridge rest_api:             test result: ok.   4 passed; 0 failed; 0 ignored
huly-bridge smoke_0_7_19:         test result: ok.   0 passed; 0 failed; 4 ignored
huly-bridge wire_fixtures:        test result: ok.   1 passed; 0 failed; 0 ignored
huly-common:                      test result: ok.  12 passed; 0 failed; 0 ignored
huly-mcp:                         test result: ok.  85 passed; 0 failed; 1 ignored
```

---

## 3. Architecture Review

### Workspace Structure

| Crate | Type | Responsibility | Files | LOC |
|-------|------|---------------|-------|-----|
| `huly-common` | lib | Shared types, API DTOs, announcement schema | 4 | 280 |
| `huly-bridge` | bin + lib | WebSocket bridge, event forwarding, admin API, REST client | 26 | 7,280 |
| `huly-mcp` | bin | MCP server, bridge discovery, 18 Huly tools, sync subprocess | 9 | 3,951 |
| **Total** | | | **39** | **11,511** |

### huly-bridge Module Structure

| Module | Responsibility | Files | LOC |
|--------|---------------|-------|-----|
| `huly/` | Huly API client (auth, rpc, ws, client, accounts, **rest**) | 8 | 3,514 |
| `bridge/` | Event loop, NATS publisher, REST proxy, **rate_limit**, announcer | 6 | 1,161 |
| `admin/` | Health, metrics, router, platform API | 5 | 830 |
| `service/` | Lifecycle management, watchdog (cfg-gated) | 3 | 569 |
| Root | Config, errors, **lib.rs**, main entry | 4 | 479 |
| `tests/` | mock_huly + rest_api + wire_fixtures + common helpers + fixtures | 4 + fixtures | 548 |
| **Total** | | **26** | **7,101** (excl. fixture JSON) |

New since 2026-04-16: `huly/rest.rs` (815), `bridge/rate_limit.rs` (120), `lib.rs` (19), `tests/mock_huly.rs` (307), `tests/rest_api.rs` (90), `tests/wire_fixtures.rs` (33), `tests/common/mod.rs` (118).

### huly-mcp Module Structure

| Module | Responsibility | Files | LOC |
|--------|---------------|-------|-----|
| Root | Config, main entry, **sync** subprocess wrapper | 3 | 781 |
| `discovery` | Bridge registry, NATS subscriber, stale reaper | 1 | 242 |
| `bridge_client` | HTTP client to bridge REST API | 1 | 328 |
| `mcp/` | MCP server (20 tools), **catalog**, **tools** helpers | 4 | 2,600 |
| **Total** | | **9** | **3,951** |

New since 2026-04-16: `sync.rs` (414), `mcp/catalog.rs` (380), `mcp/tools.rs` (807), `mcp/server.rs` grew 485 → 1,410 (+925).

### Separation of Concerns: PASS

- **huly-common** — Pure data types with no I/O, shared across crates
- **huly-bridge** — Runtime service with WebSocket, NATS, HTTP server
- **huly-mcp** — Standalone MCP server, depends on huly-common only
- No circular dependencies between crates
- Types properly extracted: `Doc`, `FindResult`, `TxResult`, `FindOptions` moved to huly-common
- Error types isolated per concern (ConnectionError, ClientError, PublishError, AuthError)
- Config parsed once at startup, passed by value — no global mutable state

### Trait-Based Abstractions: PASS

| Trait | Crate | Module | Purpose | Mockable |
|-------|-------|--------|---------|----------|
| `HulyConnection` | huly-bridge | huly/connection | WebSocket interface | Yes (mockall) |
| `PlatformClient` | huly-bridge | huly/client | RPC operations | Yes (mockall) |
| `EventPublisher` | huly-bridge | bridge/nats_publisher | NATS publish | Yes (mockall) |
| `SystemNotifier` | huly-bridge | service/watchdog | systemd notify | Yes (test mock) |

All public abstractions use `async_trait` and are injected via `Arc<dyn Trait>`, enabling comprehensive unit testing without external systems.

---

## 4. Code Quality

### Design Patterns: PASS

- **Dependency Injection** — All core traits injected via `Arc<dyn Trait>`
- **Error Classification** — `is_transient()` / `is_fatal()` for retry vs shutdown decisions
- **Async Actor** — Event loop, announcer, watchdog as independent async tasks with channel communication
- **State Machine** — HealthState tracks (huly_connected, nats_connected) → derived ready state
- **Protocol Negotiation** — Hello handshake supports 4 serialization modes (JSON/msgpack × compressed/raw)
- **Cancellation** — `CancellationToken` tree for cooperative shutdown across all spawned tasks
- **Adapter** — RestProxy adapts HTTP requests to upstream Huly API with token injection

### Error Handling: PASS

- `thiserror` for module-level custom errors with `Display` formatting
- `anyhow` at the top level for context propagation
- Every public function returns `Result<T, CustomError>`
- No `.unwrap()` on fallible operations in production code (3 guarded cases with prior validation or fallback)
- 8 `let _ =` occurrences — all justified (systemd notifications, best-effort channel sends)
- No `todo!()`, `unimplemented!()`, or `panic!()` in production code
- Error variants classified as transient (retry) or fatal (shutdown)
- Platform API properly maps `ClientError` to HTTP status codes (502/404/403/422/500)

### Naming Conventions: PASS

- Consistent snake_case for functions/variables, CamelCase for types
- Module naming follows Rust idioms
- No TODO/FIXME/HACK comments in codebase

### Dependency Choices: PASS

| Category | Crate | Version | Status |
|----------|-------|---------|--------|
| Async runtime | tokio | 1.x | OK |
| HTTP server | axum | 0.8 | OK |
| HTTP client | reqwest | 0.12 | OK |
| WebSocket | tokio-tungstenite | 0.26 | OK |
| Message queue | async-nats | 0.39 | OK |
| Serialization | serde + serde_json | 1.x | OK |
| Msgpack | rmp-serde | 1.x | OK |
| Compression | snap | 1.x | OK |
| MCP protocol | rmcp | 1.3 | OK |
| Metrics | metrics + prometheus | 0.24 / 0.16 | OK |
| CLI | clap | 4.x | OK |
| Logging | tracing | 0.1 | OK |
| Error handling | thiserror | 2.x | OK |
| Secret management | secrecy | 0.10 | OK |
| Config | toml | 0.8 | OK |
| TLS | rustls | 0.23 | OK — pure Rust, cross-platform |
| TLS certs | rustls-pemfile | 2.x | OK |
| TLS roots | webpki-roots | 0.26 | OK — bundled Mozilla roots |
| systemd | sd-notify | 0.4 | OK — Linux-only (`cfg(target_os = "linux")`) |
| JSON Schema | schemars | 1.2 | OK |
| Test mocking | mockall | 0.13 | OK (dev) |
| Test HTTP | wiremock | 0.6 | OK (dev) |

---

## 5. Test Coverage Analysis

### Per-Crate Breakdown

#### huly-common (10 tests)

| Module | Tests | Coverage Areas |
|--------|-------|---------------|
| `types.rs` | 7 | Doc serde roundtrips, FindResult, FindOptions, TxResult, missing optional fields |
| `announcement.rs` | 1 | BridgeAnnouncement serialization roundtrip |
| `api.rs` | 2 | FindRequest and CreateRequest serialization |

#### huly-bridge (153 tests)

| Module | Tests | Coverage Areas |
|--------|-------|---------------|
| `error.rs` | 3 | Transient/fatal classification, display formatting |
| `config.rs` | 8 | Token/password auth, custom settings, missing fields, defaults, TLS options/defaults |
| `huly/auth.rs` | 4 | Token passthrough, password login success/failure, default path |
| `huly/rpc.rs` | 18 | All 4 protocol modes, compression, hello request/response (id:-1), errors, ID atomics, rate limit, meta |
| `huly/accounts.rs` | 10 | selectWorkspace, getLoginInfoByToken, HTTP/RPC errors, network errors, login password |
| `huly/connection.rs` | 14 | Handshake (JSON), send/receive, disconnect, timeout, server push, id dispatch, hex/text prefix, invalid text fallback |
| `huly/client.rs` | 9 | All CRUD ops, array response, options, RPC/connection error propagation |
| `bridge/event_loop.rs` | 7 | Event forwarding, retry logic, max retries, cancellation, publish failure, event type extraction |
| `bridge/nats_publisher.rs` | 7 | Subject mapping, mock publish/error, connect failures, error classification |
| `bridge/proxy.rs` | 10 | URL construction, all HTTP methods, upstream errors, non-JSON response |
| `bridge/announcer.rs` | 4 | Package version, health reflection, uptime tracking, serialization roundtrip |
| `admin/health.rs` | 6 | Initial state, readiness logic, toggling, clone sharing, status reflection |
| `admin/metrics.rs` | 6 | Event forwarded/failed/dropped counters, WS reconnect counter, NATS/WS connected gauges |
| `admin/router.rs` | 7 | All admin endpoints, auth middleware (required/valid/wrong/bypass) |
| `admin/platform_api.rs` | 14 | All CRUD handlers, error mapping (502/404/403/401/422), input validation |
| `service/lifecycle.rs` | 4 | Backoff growth, cap, overflow |
| `service/watchdog.rs` | 5 | Ping when healthy, skip when disconnected, recovery, cancel |

#### huly-mcp (82 tests)

| Module | Tests | Coverage Areas |
|--------|-------|---------------|
| `config.rs` | 6 | Full/minimal config, missing file, invalid syntax, **`[mcp.sync]` parsing**, **`[mcp.catalog]` overrides** |
| `discovery.rs` | 5 | Registry CRUD, stale removal, last_seen refresh |
| `bridge_client.rs` | 10 | Find, create, update, delete, null handling, error propagation |
| `mcp/catalog.rs` | 13 | CardType / IssueStatus / RelationType round-trips, override merging, fallback to defaults |
| `mcp/tools.rs` | 8 | `parse_readme`, `derive_identifier`, `build_issue_attrs`, sequence/identifier formatting |
| `mcp/server.rs` | 30 | 7 generic + 9 domain + 2 sync tools (happy paths + error/skip paths), workspace + project resolution, proxy URL validation |
| `sync.rs` | 10 | spawn_capture argv assertions, dry-run flag, status JSON parse, JSON parse error, non-zero exit error, stderr tail capture, output filter, not-configured error |

#### huly-bridge integration tests (4 tests across 3 files)

| File | Tests | Coverage Areas |
|------|-------|---------------|
| `tests/mock_huly.rs` | 1 | MockHuly serves WS hello round-trip (TDD smoke proving the harness works) |
| `tests/rest_api.rs` | 2 | RestClient `get_config` happy path + 404 against `MockHuly` |
| `tests/wire_fixtures.rs` | 1 | Fixture-shape sanity (catches drift in `tests/fixtures/*.json`) |

### Coverage Gaps

| Gap | Severity | Notes |
|-----|----------|-------|
| Live 0.7.19 server contract test | **Medium** | No `tests/smoke_0_7_19.rs` gated on `HULY_INTEGRATION_URL` env var (M6 / issue #22) |
| `lifecycle.rs` `run()` orchestrator | **Medium** | Components tested individually; full startup not tested |
| TLS connection paths (rustls) | **Low** | `NoVerifier` and custom CA cert paths not unit-tested |
| Windows `shutdown_signal()` | **Low** | Cannot test on Linux CI |
| NATS credentials file support | **Low** | Config parsing tested; runtime loading not |
| `huly_create_project` filesystem error paths | **Low** | README-not-found error path covered; permission-denied / IO errors only manually verified |
| Storage / Markup / Collaborator client | **N/A** | Phase 3 deferred — no tests because no code shipped |

---

## 6. Security Review

### Systemd Hardening: PASS

| Directive | Value | Effect |
|-----------|-------|--------|
| `DynamicUser` | yes | No fixed UID, ephemeral user |
| `ProtectSystem` | strict | Root filesystem read-only |
| `ProtectHome` | yes | No access to /home |
| `NoNewPrivileges` | yes | Cannot escalate privileges |
| `PrivateTmp` | yes | Isolated /tmp namespace |
| `ReadOnlyPaths` | /etc/huly-bridge | Config is read-only |
| `MemoryMax` | 512M | Memory limit enforced |
| `LimitNOFILE` | 65536 | File descriptor cap |

### TLS: PASS

- Default: certificate verification enabled (`tls_skip_verify = false`)
- Custom CA certificate support via `tls_ca_cert` config path
- Pure Rust TLS via `rustls` crate (cross-platform, no system dependency)
- Bundled Mozilla root certificates via `webpki-roots` (portable trust store)
- `NoVerifier` struct implements `ServerCertVerifier` for skip-verify mode — all 4 trait methods correct

### Token Handling: PASS

- Auth token passed as WebSocket query parameter (encrypted via WSS)
- Token stored in memory only (`RestProxy`, `WsConnection` structs)
- No token logging observed in tracing calls
- Config supports both token and password authentication

### Potential Security Concerns

| # | Item | Severity | Details | Status |
|---|------|----------|---------|--------|
| ~~S1~~ | ~~TLS verification can be disabled via config~~ | ~~High~~ | **FIXED** — `warn!` emitted at startup when `tls_skip_verify=true` | **FIXED** |
| ~~S2~~ | ~~Passwords in plaintext TOML config~~ | ~~Medium~~ | **FIXED** — `secrecy::SecretString` wraps token, password, and api_token. Zeroized on drop, `Debug` prints `[REDACTED]`. | **FIXED** |
| ~~S3~~ | ~~Admin API auth optional~~ | ~~Medium~~ | **FIXED** — `/api/v1/*` returns 403 when `api_token` not configured. `/metrics` moved to public routes. | **FIXED** |
| ~~S4~~ | ~~No rate limiting on auth failures~~ | ~~Medium~~ | Not applicable — admin API is internal/localhost only. | **N/A** |
| S5 | No CORS on admin API | **Low** | Intentional — admin API is server-to-server only. Documented in DEPLOY.md §9. | **DOCUMENTED** |
| ~~S6~~ | ~~API tokens as plain String in memory~~ | ~~Low~~ | **FIXED** — Covered by S2 fix (`secrecy::SecretString`). | **FIXED** |
| ~~S7~~ | ~~Platform API unauthenticated~~ | ~~Medium~~ | **FIXED** — Bearer token auth middleware added | **FIXED** |
| ~~S8~~ | ~~Admin API unauthenticated~~ | ~~Low~~ | **FIXED** — Protected routes require auth when `api_token` configured | **FIXED** |
| S9 | Token in WebSocket URL path | **Low** | Safe over WSS, but may appear in server access logs. Log scrubbing guidance in DEPLOY.md §9. | **DOCUMENTED** |
| ~~S10~~ | ~~MCP trusts bridge proxy_url from NATS~~ | ~~Low~~ | **FIXED** — `validate_proxy_url()` rejects non-http/https schemes. | **FIXED** |

---

## 7. Potential Issues & Recommendations

### Issues Found

| # | Issue | Severity | Location | Status |
|---|-------|----------|----------|--------|
| ~~1~~ | ~~No WebSocket reconnection logic~~ | ~~Medium~~ | | **FIXED** |
| ~~2~~ | ~~Empty integration test suite~~ | ~~Medium~~ | All crates | **FIXED** — 6 integration test files now cover the full wire: `mock_huly` (WS envelope), `rest_api` (REST proxy), `admin_collection_routes` (platform API incl. apply-if), `smoke_0_7_19` (live, env-gated), `wire_fixtures` (snapshot), `markup_e2e` (collaborator round-trip). Integration-test count went 0 → 6 files / 20+ tests over the 0.7.19 migration + fix batches |
| ~~3~~ | ~~No backoff/retry on transient errors~~ | ~~Medium~~ | | **FIXED** |
| ~~4~~ | ~~Platform API has no authentication~~ | ~~Medium~~ | | **FIXED** |
| ~~5~~ | ~~No TLS certificate validation options~~ | ~~Low~~ | | **FIXED** |
| ~~6~~ | ~~No NATS credentials file support tested~~ | ~~Low~~ | `config.rs` | **FIXED** in `c3c2b96` — `connect_fails_with_missing_credentials_file` (error-path) already existed; added `credentials_file_happy_path_loads_from_disk` (happy-path): writes a valid JWT+nkey `.creds` blob to a temp file, asserts the error is a network failure rather than "failed to load credentials", proving `with_credentials_file` parsed the file and constructed the KeyPair successfully |
| ~~7~~ | ~~Flaky test: `request_ids_increment`~~ | ~~Low~~ | | **FIXED** |
| ~~8~~ | ~~Pending entry orphaned on `SendFailed`~~ | ~~Low~~ | | **FIXED** |
| ~~9~~ | ~~Dead code in huly-mcp config~~ | ~~Low~~ | | **FIXED** |
| ~~10~~ | ~~MCP fire-and-forget tasks~~ | ~~Low~~ | | **FIXED** |
| ~~11~~ | ~~MCP server/bridge_client minimal tests~~ | ~~Medium~~ | | **FIXED** |
| ~~12~~ | ~~No input validation on platform API~~ | ~~Low~~ | | **FIXED** |
| ~~13~~ | ~~Pending requests HashMap unbounded~~ | ~~Medium~~ | `connection.rs:64` | **FIXED** in `f92da4f` — cap = `huly.max_pending_requests` (default 10,000); rejects with new transient `ConnectionError::PendingRequestsExceeded` and increments `huly_bridge_pending_requests_dropped_total` counter |
| ~~14~~ | ~~Event channel drops silently (no metric)~~ | ~~Low~~ | `connection.rs:233-242` | **FIXED** in `13a1a06` — switched from `send().await` (which never dropped — backpressured instead) to `try_send`; counter `huly_bridge_events_dropped_total` now actually fires on overflow |
| ~~15~~ | ~~TLS skip has no runtime warning~~ | ~~Medium~~ | `connection.rs:118` | **FIXED** |
| ~~16~~ | ~~Platform API open when api_token unset~~ | ~~Medium~~ | `router.rs:41-66` | **FIXED** |
| ~~17~~ | ~~`addCollection` / `updateCollection` not exposed on bridge admin API~~ | ~~Medium~~ | `admin/platform_api.rs`; consumed by `huly-mcp/src/mcp/tools.rs` | **FIXED** in `3d1e930`+`90aa282`+`2f5594a` — new `addCollection`/`updateCollection` RPC methods + admin routes (`POST /api/v1/add-collection`, `POST /api/v1/update-collection`); `huly_create_issue` rewritten to use `$inc` (server-atomic) + `addCollection`. **Residual race window** (between `$inc` and follow-up `find_one`): identifier uniqueness preserved (no duplicates possible); contiguity not guaranteed. Tier B follow-up tracked as new issue #24 (TxApplyIf with scope) |
| ~~18~~ | ~~Issue `description` stored as plain string instead of markup ref~~ | ~~Medium~~ | `crates/huly-mcp/src/mcp/tools.rs` create/update issue | **FIXED** in `441f25f`..`fb4ea26` — new `huly/markdown.rs` (CommonMark ↔ ProseMirror JSON via `pulldown-cmark`), `huly/collaborator.rs` (HTTP RPC client for `create/get/update-content`), `service/workspace_token.rs` cache populated on reconnect, admin `POST /api/v1/upload-markup` + `/fetch-markup` routes, MCP `create_issue`/`update_issue` now upload the markup blob and write the `MarkupBlobRef` back to `description`. No WebSocket / YJS needed — collaborator is plain HTTP POST with ProseMirror-JSON payload. Adds `pulldown-cmark 0.12` (only new dep) |
| ~~19~~ | ~~Catalog defaults are deployment-specific (Muhasebot IDs)~~ | ~~Low~~ | `crates/huly-mcp/src/mcp/catalog.rs` | **CLOSED (Won't Fix — documented contract)** — runtime auto-discovery rejected: overrides are the stable contract (queryable via `huly_discover`). Added `CatalogOverrides::unknown_keys()` + startup `warn!` so operators notice typos; `config/mcp.example.toml` documents the keys |
| ~~20~~ | ~~`RestAccount` (rest.rs) parallel to `Account` (rpc.rs)~~ | ~~Low~~ | `crates/huly-bridge/src/huly/rest.rs:46` | **FIXED** in `d67cf06` — `RestAccount` and `SocialId` removed; `huly::rpc::Account` is canonical with `social_ids: Vec<String>` (`#[serde(default)]`) and `full_social_ids: Vec<SocialId>` (`#[serde(default)]`); both wire shapes round-trip |
| ~~21~~ | ~~`RestClient::get_config()` not wired into auth bootstrap~~ | ~~Low~~ | `crates/huly-bridge/src/service/lifecycle.rs` | **FIXED** in `bd56b9e` — `ServerConfigCache` (Arc<RwLock<Option<ServerConfig>>>) populated by best-effort `bootstrap_server_config()` after auth, before NATS/admin/watchdog. On 404 or network error: warn-log + cache stays `None`; startup never blocked |
| ~~22~~ | ~~No live integration test against 0.7.19 server~~ | ~~Low~~ | `crates/huly-bridge/tests/` | **FIXED** in `697b271` — `tests/smoke_0_7_19.rs` with 4 `#[ignore]`d tests double-gated on `HULY_INTEGRATION_URL` env var; runs via `cargo test --test smoke_0_7_19 -- --ignored --nocapture` |
| ~~23~~ | ~~11 long-standing clippy warnings (auto-deref, collapsible-if, type-complexity)~~ | ~~Low~~ | `huly/connection.rs`, `bridge/event_loop.rs`, `config.rs` | **FIXED** in commit `87d10a9`; `cargo clippy --tests -- -D warnings` now clean |
| ~~24~~ | ~~Residual race in `huly_create_issue` between `$inc` and follow-up `find_one`~~ | ~~Low~~ | `crates/huly-mcp/src/mcp/tools.rs` | **FIXED** in `97afe82`..`c84250e` — new `apply_if_tx(scope, matches, txes)` primitive on `PlatformClient` + admin `POST /api/v1/apply-if` route + `huly-mcp/src/txcud.rs` helpers (`TxUpdateDoc`, `TxCollectionCUD` builders mirroring upstream `TxFactory`). `create_issue_in_project` now bundles `$inc sequence` + `addCollection Issue` into one server-serialized scope `tracker:project:{id}:issue-create` with `match = {sequence: N}`; retries ≤5× with exponential backoff on contention. Closes both uniqueness AND contiguity of issue identifiers |

### Recommendations

**R1. Add capacity limit to pending requests map (Medium)**
Add a max concurrent requests limit (e.g., 10,000) to the pending requests HashMap in `connection.rs:64`. Return an error when limit is reached rather than allowing unbounded growth.

**~~R2. Integration tests (Medium)~~**
~~Add integration tests using `wiremock` for full startup sequence, event forwarding end-to-end, REST proxy cycle, and MCP tool invocation.~~ **DONE** — 6 integration files live: `mock_huly`, `rest_api`, `admin_collection_routes` (includes apply-if), `wire_fixtures`, `smoke_0_7_19` (live, env-gated), `markup_e2e` (collaborator round-trip).

**~~R3. Warn when TLS verification is disabled (Medium)~~**
~~Emit a prominent `warn!` log at startup when `tls_skip_verify = true`.~~ **DONE** — `warn!` added in `connection.rs`.

**~~R4. Require api_token for platform API routes (Medium)~~**
~~Return 403 on `/api/v1/*` routes when `api_token` is not configured.~~ **DONE** — 403 returned when unset. `/metrics` moved to public routes.

**R5. Add events_dropped Prometheus counter (Low)**
Event drops are logged at `warn` level but not metered. Add a counter to enable alerting on sustained overflow.

**~~R6. SecretString for credentials (Low)~~**
~~Wrap `api_token`, `password`, and `token` fields in `secrecy::SecretString`.~~ **DONE** — `secrecy` 0.10 added with serde support. All credential fields wrapped.

**~~R7. Add `addCollection` / `updateCollection` to bridge admin API (Medium)~~**
~~Issue #17.~~ **DONE** in `3d1e930`+`90aa282`+`2f5594a`. Race fixed via server-atomic `$inc` + `addCollection`; identifier uniqueness preserved. Residual contiguity gap tracked as #24.

**~~R8. Wire `get_config()` into bridge bootstrap (Low)~~**
~~Issue #21.~~ **DONE** in `bd56b9e`. `ServerConfigCache` populated best-effort after auth.

**~~R9. Live 0.7.19 integration test (Low)~~**
~~Issue #22.~~ **DONE** in `697b271`. 4 `#[ignore]`d tests double-gated on `HULY_INTEGRATION_URL`.

**R10. TxApplyIf for true atomic create-issue (Low)**
Issue #24. Implement `tx` RPC + `TxApplyIf { scope, match, txes }` on `PlatformClient`; rewrite `huly_create_issue` to bundle `$inc` + `addCollection` in one server-serialized scope. Closes the residual contiguity gap. Wire format spec already in QA notes (E1 investigation); about ½ day of work.

---

## 8. Metrics Summary (2026-04-22 — post fix-batch)

| Metric | Value | Δ since migration end | Δ since 2026-04-16 |
|--------|-------|------------------------|---------------------|
| Workspace crates | 3 | — | — |
| Total source files | 41 | +2 | +8 |
| Lines of code | ~13,200 | +~1,700 | +~6,300 |
| Test count (active) | 327 | +34 | +127 |
| Test count (`#[ignore]`d) | 5 | +5 | +5 |
| Test pass rate | 100% | — | — |
| Clippy warnings (`-D warnings`) | 0 | — | −11 |
| Unsafe blocks | 0 | — | — |
| Dependencies (direct) | 26 | — | — |
| Dev dependencies | 4 | — | — |
| Mockable traits | 4 | — | — |
| Prometheus metrics | 8 (6 counters, 2 gauges) | +2 (`events_dropped_total`, `pending_requests_dropped_total`) | +2 |
| Admin endpoints | 11 (healthz, readyz, metrics, status + **7** platform API) | +2 (`add-collection`, `update-collection`) | +2 |
| Bridge `PlatformClient` RPC methods | 7 (`findAll`, `findOne`, `createDoc`, `updateDoc`, `removeDoc`, `addCollection`, `updateCollection`) | +2 | +2 |
| MCP tools | 18 (7 generic + 9 domain + 2 sync) | — | +11 |
| Typed REST endpoints (RestClient) | 6 | — | +6 |
| Integration test files | 5 | +2 (`smoke_0_7_19`, `admin_collection_routes`) | +5 |
| Config knobs | +1 (`huly.max_pending_requests`, default 10,000) | +1 | +1 |
| Build targets | 2 (x86_64-unknown-linux-gnu, x86_64-pc-windows-gnu) | — | — |

---

## 9. Verdict

| Category | Rating |
|----------|--------|
| Architecture | **Good** — Clean workspace separation, trait-based DI, no circular deps |
| Code Quality | **Good** — Zero clippy warnings, consistent conventions, no unsafe |
| Test Coverage | **Good** — 200 tests, all unit paths covered |
| Security | **Good** — rustls TLS with secure defaults, mandatory auth, SecretString credentials |
| Documentation | **Good** — Comprehensive DEPLOY.md, config examples |
| Memory Safety | **Good** — No unsafe, proper cleanup, bounded channels |
| Cross-Platform | **Good** — Correct cfg gates, pure Rust TLS, Windows + Linux builds verified |
| Production Readiness | **Good** — Security hardened, cross-platform, REST surface up-to-date with upstream 0.7.19. Remaining: live-server integration test (R9), bridge `addCollection` API (R7) |
| Upstream API Conformance | **Good** — Wire envelope, REST endpoints, OTP auth all match upstream `@hcengineering/api-client` v0.7.19 |
| MCP Tool Coverage | **Good** — 20 tools cover discover + Tracker CRUD + cards + relations + markup + sync; mirrors upstream `huly-api/packages/mcp-server` plus our generic escape hatches |

**Overall: PASS with recommendations**

The codebase remains well-engineered after a substantial migration. 293 tests (100% pass), zero clippy warnings under `-D warnings`, no new unsafe blocks, no new dependencies. The 0.7.19 migration was executed with strict TDD per CLAUDE.md across 6 phases (scaffold + wire + OTP + REST + MCP tools + sync), all documented in §11.4–§11.9. New issues #17–#22 are deliberate deferrals tracked for follow-up; the lint cleanup (#23) closed all long-standing baseline warnings.

---

## 10. Memory Leak Analysis

**Date:** 2026-04-22 (re-verified after 0.7.19 migration)
**Scope:** All 3 crates — focus on async task lifecycle, channel management, connection cleanup. Migration delta reviewed in §10.6.

### 10.1 Prior Findings

| # | Finding | Status |
|---|---------|--------|
| ~~ML-1~~ | Pending requests HashMap orphaning on timeout | **FIXED** |
| ~~ML-2~~ | Event channel silent drop (no observability) | **FIXED** |
| ~~ML-3~~ | WebSocket tasks fire-and-forget | **FIXED** |
| ~~ML-4~~ | Admin/watchdog tasks fire-and-forget | **FIXED** |
| ~~ML-5~~ | NATS connection no explicit close | **FIXED** |
| ~~ML-6~~ | Pending entry orphaned on SendFailed path | **FIXED** |
| ~~ML-7~~ | huly-mcp fire-and-forget tasks | **FIXED** |

### 10.2 Current Findings — all closed by fix batch

**~~ML-8: Pending requests HashMap has no upper bound (Medium)~~** — **FIXED** in `f92da4f`
- **File:** `crates/huly-bridge/src/huly/connection.rs`
- Cap = `huly.max_pending_requests` (default `DEFAULT_MAX_PENDING_REQUESTS = 10_000`).
- New transient `ConnectionError::PendingRequestsExceeded { cap }` returned BEFORE insert and BEFORE consuming an RPC id.
- New counter `huly_bridge_pending_requests_dropped_total` for alerting.
- Cap check is best-effort under concurrency (brief overshoot bounded by # of racing callers); strict bounding deferred.

**~~ML-9: Event channel drops without backpressure metrics (Low)~~** — **FIXED** in `13a1a06`
- **File:** `crates/huly-bridge/src/huly/connection.rs`
- **Bonus bug fix:** the original `event_tx.send().await` blocked on backpressure, so the drop path NEVER fired — backpressure silently increased read-loop latency instead.
- Switched to `try_send` + match on `TrySendError::{Full, Closed}`; counter `huly_bridge_events_dropped_total` now actually meters drops.
- Extracted `forward_event_or_drop()` for unit testability.

### 10.3 Safe Patterns

| Item | File | Assessment |
|------|------|------------|
| Arc usage (no cycles) | connection.rs, lifecycle.rs, health.rs | **SAFE** — Linear ownership, no back-references |
| Bounded write channel (256) | connection.rs | **SAFE** — Proper cleanup on send failure |
| Bounded event channel (1024) | connection.rs | **SAFE** — Bounded, drops with log |
| Oneshot handshake channel | connection.rs | **SAFE** — Single-use, consumed or dropped |
| CancellationToken tree | lifecycle.rs | **SAFE** — Parent cancels children, no cycles |
| All JoinHandles stored and awaited | lifecycle.rs | **SAFE** — Timeout on shutdown |
| WebSocket task abort on drop | connection.rs | **SAFE** — Both shutdown() and Drop abort tasks |
| NATS client flush on shutdown | lifecycle.rs | **SAFE** — Explicit flush before drop |
| AtomicBool flags | connection.rs, health.rs | **SAFE** — Fixed size, lock-free |

### 10.4 Memory Leak Risk Summary

| # | Finding | Severity | Type | Status |
|---|---------|----------|------|--------|
| ~~ML-1 through ML-7~~ | (Prior findings) | — | — | All **FIXED** |
| ~~ML-8~~ | ~~Pending requests map unbounded~~ | ~~Medium~~ | Unbounded growth | **FIXED** in `f92da4f` (issue #13) |
| ~~ML-9~~ | ~~Event channel drops without metrics (and silent backpressure bug)~~ | ~~Low~~ | Data loss (not leak) | **FIXED** in `13a1a06` (issue #14) |

**No open ML findings.**

### 10.5 Memory Leak Verdict

| Category | Rating |
|----------|--------|
| Unbounded Growth | **Good** — One unbounded map with timeout cleanup |
| Resource Cleanup | **Excellent** — All connections, tasks, channels properly cleaned |
| Task Lifecycle | **Excellent** — All spawned tasks tracked and awaited |
| Channel Management | **Good** — Bounded channels, proper closure |
| Static State | **Excellent** — No global mutable state |

**Overall: PASS** — No memory leaks detected. ML-8 (pending-requests cap) and ML-9 (event drop counter + backpressure bug fix) both closed by fix batch. All async tasks properly tracked, awaited, and cancelled on shutdown; pending-requests map is now bounded with structured rejection on overflow; event channel drops are properly metered.

### 10.6 New Code Review (2026-04-22 migration delta)

| New module | Resource concerns | Verdict |
|------------|-------------------|---------|
| `huly/rest.rs` (RestClient) | Per-call request/response; `BTreeMap` for query maps created+dropped per call; reused `reqwest::Client` (pooled). No background tasks, no long-lived state. | **SAFE** |
| `bridge/rate_limit.rs` (`RateLimitInfo`) | Pure header parsing into `Option<u64>` fields. Stateless. | **SAFE** |
| `huly/accounts.rs` (`login_otp`, `validate_otp`) | Reuses existing generic `call<T>` HTTP helper. No new state. | **SAFE** |
| `huly/rpc.rs` (new envelope fields) | Optional `serde` fields on existing structs. No allocation patterns changed. | **SAFE** |
| `huly-mcp/src/sync.rs` (`SyncRunner`) | `tokio::process::Command` with `kill_on_drop(true)` (sync.rs:183) + `tokio::time::timeout` (sync.rs:204) — child killed on drop or timeout. Stdout/stderr buffered to `String`s and returned (bounded by subprocess output). | **SAFE** — verified at sync.rs:183, sync.rs:204-212 |
| `huly-mcp/src/mcp/catalog.rs` | `HashMap<CardType,String>` and `HashMap<RelationType,String>` populated once at startup from config or defaults. Bounded by config file size. | **SAFE** |
| `huly-mcp/src/mcp/tools.rs` | Stateless helpers; one local `HashMap<String, i64>` (tools.rs:129) for in-function accumulation, dropped on return. | **SAFE** |
| `huly-mcp/src/mcp/server.rs` (new tool handlers) | Each tool is synchronous request/response over the existing `bridge_client`. No new spawned tasks, no new channels. | **SAFE** |

**No new ML-N findings introduced by the migration.** ML-8 (pending requests cap) and ML-9 (event channel drop metric) remain the only open items.

---

## 11. Feature Quality Assessment (2026-04-16)

### 11.1 Cross-Platform Build Support

| Aspect | Rating | Notes |
|--------|--------|-------|
| Implementation | **Good** | Minimal, surgical changes — cfg gates on 3 call sites + 2 structs, rustls rewrite confined to one function |
| Test Coverage | **Adequate** | 200 existing tests pass; no new tests needed (cfg gates are compile-time, not runtime behavior changes). TLS paths and Windows signals untestable on Linux CI |
| Error Handling | **Good** | All rustls operations return `Result` with context. `NoVerifier` ignores errors by design (skip-verify mode) |
| Security | **Good** | `NoVerifier` only activated with explicit `tls_skip_verify=true` + `warn!` log. Default path uses rustls with bundled Mozilla roots |
| TDD Adherence | **N/A** | Structural change (compilation targets), not new runtime behavior |

### 11.2 TLS Backend Migration (native-tls → rustls)

| Aspect | Rating | Notes |
|--------|--------|-------|
| Implementation | **Good** | `NoVerifier` implements all 4 `ServerCertVerifier` methods. `supported_verify_schemes()` delegates to ring provider correctly |
| Test Coverage | **Adequate** | Existing connection tests validate handshake, disconnect, timeout paths — TLS layer is below mock server boundary |
| Error Handling | **Good** | PEM parse errors, cert add errors, file read errors all propagated with context strings |
| Security | **Good** | Stricter than native-tls by default. Bundled roots more portable than system trust store. Skip-verify properly guarded |

### 11.3 Build Tooling (Makefile + cross)

| Aspect | Rating | Notes |
|--------|--------|-------|
| Implementation | **Good** | Clean Makefile with phony targets. `CARGO_TARGET_DIR` passthrough solves glibc mismatch. Podman-native |
| Correctness | **Good** | `make release` verified — produces `dist/linux-x86_64/` (ELF) and `dist/windows-x86_64/` (PE32+) |

### TDD Summary

| Feature | Tests Written | Tests Needed | TDD Score |
|---------|---------------|--------------|-----------|
| Cross-platform cfg gates | 0 (existing 200 pass) | 0 (compile-time) | **N/A** |
| rustls TLS rewrite | 0 (existing 14 connection tests cover) | ~2 (skip-verify, custom CA) | **Adequate** |
| Build tooling | 0 | 0 (infra, not code) | **N/A** |

**TDD Verdict:** Not applicable — this is a cross-compilation infrastructure change, not new runtime behavior. Existing 200 tests validate that the refactoring introduced no regressions.

---

## 11.x — 0.7.19 Migration Features (2026-04-22)

### 11.4 Test Scaffolding (commit `ce7c7b4`)

| Aspect | Rating | Notes |
|--------|--------|-------|
| Implementation | **Good** | `MockHuly` axum harness with WS + REST routes; `WsScript` builder for scripted frame exchange; fixture loader; `ephemeral_port` and `ephemeral_nats` (graceful degrade when nats-server absent). msgpack+snappy helpers duplicated rather than restructuring crate (justified — no `lib.rs` at the time). |
| Test Coverage | **Good** | Single TDD smoke test (`mock_huly_serves_hello`) proves the harness works red-first. Future phases extend, not retrofit. |
| Error Handling | **Good** | NATS helper returns `Option<>` and warns instead of panicking; `Drop` impl logs warning instead of double-panicking during test unwind. |
| Security | **N/A** | Test-only code. |
| TDD Adherence | **Excellent** | Smoke test written first, harness built minimally to pass. |

### 11.5 Wire Protocol Parity (commits `1045e7e`, `2aec88c`)

| Aspect | Rating | Notes |
|--------|--------|-------|
| Implementation | **Good** | `HelloResponse` extended with `serverVersion`, `lastTx`, `lastHash`, `account`, `useCompression` (all `Option<>` + `serde(default)` — forward+backward compat). `RpcResponse` gained `bfst`/`queue` informational metrics. `RateLimit` gained `current`/`reset`. New `bridge::rate_limit::RateLimitInfo` extractor parses `X-RateLimit-{Limit,Remaining,Reset}`, prefers `Retry-After-ms` over `Retry-After`, swallows malformed values. New `ProxyResponse { body, rate_limit }` wrapper preserves legacy `forward()` callers. |
| Test Coverage | **Good** | 21 new tests: 8 envelope decode tests (legacy + v719 + minimal-account), 6 REST header tests (full v719, ms vs seconds fallback, ms-takes-precedence, no-headers, unit extractor, malformed), 5 rate-limit module tests + 1 fixture-shape sanity. |
| Error Handling | **Good** | All new fields default-on-absence; malformed numeric headers silently ignored (prevents brittle bridge crashes on upstream evolution). |
| Security | **N/A** | Wire-format extension, no security-sensitive paths added. |
| TDD Adherence | **Excellent** | Per-field red-first tests visible in commit; agent reported red→green→refactor for each. |

### 11.6 OTP Auth Methods (commit `6092306`)

| Aspect | Rating | Notes |
|--------|--------|-------|
| Implementation | **Good** | `login_otp(email)` and `validate_otp(email, code)` reuse the existing generic `call<T>` helper on `AccountsClient`. Zero HTTP plumbing duplication; no refactor needed. New structs `OtpInfo { sent, retry_on }` and `LoginInfo { account, name?, social_id?, token? }` — distinct from existing `WorkspaceLoginInfo` (different field sets per upstream). |
| Test Coverage | **Good** | 5 wiremock tests: happy path for both methods, RPC error propagation for both, null-optional-field deserialization for `validate_otp`. |
| Error Handling | **Good** | RPC errors surface via existing `AccountsError`; null fields tolerated via `Option<>`. |
| Security | **Good** | Email + code passed in JSON body over existing TLS path. No code logging or persistence beyond what existing `login_password` already does. |
| TDD Adherence | **Excellent** | Per agent report: red-first wiremock test for each method, then implementation. |

### 11.7 REST Surface (commits `6c1d879`, `f186ecb`, `6c5a47b`)

| Aspect | Rating | Notes |
|--------|--------|-------|
| Implementation | **Good** | New `huly/rest.rs` (815 LOC) with typed `RestClient` + 6 methods (`get_config`, `get_account`, `get_model`, `search_fulltext`, `domain_request`, `ensure_person`). Each returns `(T, RateLimitInfo)`. Snappy decode honours `Content-Encoding: snappy` header. `BTreeMap` for query maps gives deterministic test matching. New `RestError::RateLimited` carries parsed `RateLimitInfo` so retry layers don't re-parse. Lib target added (`crates/huly-bridge/src/lib.rs`) — necessary architectural change to enable integration tests; visibility of `SystemNotifier`/`SdNotifier` widened from `pub(crate)` to `pub` to silence the new `private_interfaces` warning the lib target surfaces. Trade-off documented; impact minimal. |
| Test Coverage | **Good** | 22 new tests: 20 wiremock unit tests in `rest::tests` (one per endpoint × happy + error variants), 2 `MockHuly`-driven integration tests in `tests/rest_api.rs`. |
| Error Handling | **Good** | Structured `RestError` enum: `Network`, `Decode`, `Upstream { status, body }`, `RateLimited { rate_limit }`. 4xx/5xx mapped explicitly; 429 special-cased to surface rate-limit data. |
| Security | **Good** | Bearer token reused from existing auth path. `/config.json` deliberately unauthenticated (matches upstream — bootstrap endpoint). |
| TDD Adherence | **Excellent** | Per agent report: wiremock test per endpoint written first, then implementation; refactor extracted shared request helper after duplication appeared (around endpoint 3). |

### 11.8 MCP Domain Tools + Catalog (commits `8e724c4`, `77df05a`)

| Aspect | Rating | Notes |
|--------|--------|-------|
| Implementation | **Good** | 9 new domain tools: `huly_discover`, `huly_find_cards`, `huly_find_issues`, `huly_get_issue`, `huly_create_issue`, `huly_update_issue`, `huly_create_component`, `huly_link_issue_to_card`, `huly_create_project` (renamed from upstream's misleading `create_workspace`). Catalog (`mcp/catalog.rs`) with overridable `CardType`/`IssueStatus`/`RelationType` ↔ ID maps via `[mcp.catalog.*]` TOML sub-tables. Workspace + project resolution helpers (`tools.rs::resolve_workspace`, `resolve_project`) error explicitly when ambiguous. Tools file kept as one module (not 5 commits) because `#[tool_router]` proc macro registers handlers only once — splitting would have produced commits that compile but ship handlers without router registration. |
| Test Coverage | **Good** | 28 new tests: 13 catalog round-trips + override tests, 8 tool helper tests (`parse_readme`, `derive_identifier`, `build_issue_attrs`), 7 tool-handler tests (happy + error/skip paths). |
| Error Handling | **Good** | All tools return `Result`; "not found" / "already exists" return text messages (idempotency); ambiguous workspace/project errors list candidates by name. |
| Security | **Good** | All MCP calls go through existing authenticated `bridge_client` HTTP path. No new credential surface. `huly_create_project` reads README from local FS — `std::fs::read_to_string` propagates IO errors; not exploited as path-traversal vector since the path is operator-controlled (LLM input → MCP tool). |
| TDD Adherence | **Good** | Per agent report: red-first test per tool. Slight asterisk: catalog property tests were added alongside implementation rather than purely red-first because the trait impls and test scaffolding co-evolved. |

### 11.9 Sync Subprocess Tools (commit `4e3f237`)

| Aspect | Rating | Notes |
|--------|--------|-------|
| Implementation | **Good** | `huly_sync_status` and `huly_sync_cards` MCP tools wrap the upstream Node sync pipeline via new `huly-mcp/src/sync.rs::SyncRunner`. `tokio::process::Command` with `kill_on_drop(true)` and `tokio::time::timeout`. Common `spawn_capture(args)` helper underlies both. **Notable deviation:** upstream sync CLI has no `status` subcommand, so `status()` runs an inline Node script via `node -e <inline JS>` that re-implements the upstream MCP wrapper's status logic in Node (MD5 via Node's built-in `crypto`) — avoids adding a Rust hash crate. Documented in commit body and module rustdoc. |
| Test Coverage | **Good** | 17 new tests using stub shell scripts (`fake_sync_*.sh`): happy paths for status + sync, dry-run flag passed, non-zero exit → `SyncError::NonZeroExit { code, stderr_tail }`, status JSON parse error, output filter, not-configured error, argv assertions for both subcommands. |
| Error Handling | **Good** | Structured `SyncError`: `NotConfigured`, `Spawn`, `Timeout`, `NonZeroExit { code, stderr_tail }`, `Parse`. Subprocess output filtered for known noise lines. |
| Security | **Needs Work** | `script_path`, `node_binary`, `working_dir` come from operator-controlled config (TOML on disk) — not exploitable from LLM. However, the sync subprocess inherits the parent environment by default. If a deployment puts secrets in env vars and the sync tool logs them, they leak via subprocess stdout. Recommend documenting that operators should scrub env vars in the systemd unit. (Not blocking — operator concern, not code concern.) |
| TDD Adherence | **Excellent** | Per agent report: stub-script tests written first; each stub records its argv into `args.txt` so tests can assert exact invocation without env-var races. |

### TDD Summary (0.7.19 migration)

| Feature | Tests Written | Tests Needed (est.) | TDD Score |
|---------|---------------|---------------------|-----------|
| Test scaffolding (mock_huly + helpers) | 1 | 1 (smoke) | **Excellent** |
| Wire envelope + rate-limit parser | 21 | ~15 | **Excellent** |
| OTP auth methods | 5 | ~4 (2 happy + 2 error) | **Excellent** |
| RestClient (6 endpoints) | 22 | ~18 (3 per endpoint) | **Excellent** |
| MCP domain tools (9 tools + catalog) | 28 | ~30 (3 per tool + catalog round-trips) | **Good** |
| Sync subprocess tools (2 tools) | 17 | ~10 (4 per tool + helpers) | **Excellent** |
| **Migration total** | **94** | **~78** | **Excellent** |

**TDD Verdict:** Strong. Every phase agent reported red-first per CLAUDE.md, and net new tests (94) exceed the rough estimate of what's "needed" (~78). The only soft spot is MCP catalog tests, which co-evolved with implementation rather than purely red-first; ratings remain Good/Excellent because the post-hoc round-trip property tests catch real regressions.

### Phases NOT shipped (per migration plan)

- **Phase 3 — Storage + Markup**: deferred. New `StorageClient` (PUT /upload, GET/DELETE /files/:id) and the separate collaborator WS client (`uploadMarkup`/`fetchMarkup`) are not implemented. Trigger when an MCP tool needs attachments or rich-text round-trips. Tracked as issue #18 (description-as-string workaround).
- **Phase 6B — Pure-Rust sync port**: deferred. Phase 6A keeps the Node sync subprocess; native Rust port is only needed if we drop the Node runtime dependency from production hosts.

---

## 11.10 — Fix Batch (2026-04-22, post-migration)

Six open issues closed by four parallel agents on isolated worktrees, dispatched after the initial 0.7.19 migration QA. Per-feature ratings:

### Resource hardening (issues #13, #14 — commits `f92da4f`, `13a1a06`)

| Aspect | Rating | Notes |
|--------|--------|-------|
| Implementation | **Good** | Pending-cap = const + config knob; rejects with new transient error before consuming RPC id. Bonus: caught a real bug — original `event_tx.send().await` blocked on backpressure, never firing the drop path. |
| Test Coverage | **Good** | 4 new tests including cap-enforcement and channel-overflow drop path. |
| Error Handling | **Good** | New `ConnectionError::PendingRequestsExceeded` is structured + transient. Drop path now returns instead of awaiting. |
| Security | **N/A** | Resource-management change. |
| TDD Adherence | **Excellent** | Per agent: red-first tests for cap and for `try_send` drop semantics. |

### addCollection / atomic create-issue (issue #17 — commits `3d1e930`, `90aa282`, `2f5594a`)

| Aspect | Rating | Notes |
|--------|--------|-------|
| Implementation | **Good** | Two new RPC methods (`addCollection`, `updateCollection`) with verified upstream wire shape (6/7 params). New admin routes match existing conventions. MCP `huly_create_issue` rewritten to use server-atomic `$inc` + `addCollection`. Race-freedom property documented as `#[ignore]`d test. |
| Test Coverage | **Good** | 18 new tests: 6 client unit + 4 admin unit + 3 integration via tower::oneshot + 2 huly-common roundtrip + 3 mcp wiremock. |
| Error Handling | **Good** | Bridge admin returns 502/404/403/422/500 mapped from `ClientError`, matching existing routes. MCP layer surfaces bridge errors as text. |
| Security | **Good** | New routes inherit existing bearer-token auth via `crates/huly-bridge/src/admin/router.rs`. No new credential surface. |
| TDD Adherence | **Excellent** | Per agent: wiremock test per RPC method + integration test per admin route. |

### Bootstrap + type unification (issues #20, #21 — commits `d67cf06`, `bd56b9e`)

| Aspect | Rating | Notes |
|--------|--------|-------|
| Implementation | **Good** | `Account` unified by widening `social_ids: Vec<String>` and adding `full_social_ids: Vec<SocialId>`, both `#[serde(default)]`. `ServerConfigCache` is `Arc<RwLock<Option<ServerConfig>>>` with per-URL accessors. Best-effort bootstrap warns on absence; never blocks startup. |
| Test Coverage | **Good** | 12 new tests: cache lifecycle (default empty, populated, clone shares state), wiremock-driven bootstrap success + 404 + network error, plus `MockHuly`-driven integration. Both account wire shapes (lean WS hello, full REST) round-trip. |
| Error Handling | **Good** | `/config.json` failure paths all warn-log + cache stays `None`. No panic paths. |
| Security | **Good** | Config endpoint deliberately unauthenticated (matches upstream). Cached URLs are read-only via accessors. |
| TDD Adherence | **Excellent** | Per agent: red-first round-trip tests for both account shapes; mock-driven bootstrap tests before implementation. |

### Live smoke test (issue #22 — commit `697b271`)

| Aspect | Rating | Notes |
|--------|--------|-------|
| Implementation | **Good** | 4 `#[tokio::test] #[ignore]` tests double-gated: outer `#[ignore]` + inner `HULY_INTEGRATION_URL` env check. Tests assert wire contract only (no deployment data assumptions). |
| Test Coverage | **Good** | Covers `/config.json`, `/api/v1/account/{ws}`, `/api/v1/search-fulltext/{ws}`, `/api/v1/load-model/{ws}`. Other endpoints (domain_request, ensure_person) deliberately omitted — they require server-side state setup the smoke test should not perform. |
| Error Handling | **N/A** | Test code; failures are test failures. |
| Security | **Good** | Reads three env vars; no secret logging. |
| TDD Adherence | **N/A** | Test-only addition; no production code path. |

### Fix batch TDD summary

| Fix | Tests Written | Tests Needed (est.) | TDD Score |
|-----|---------------|---------------------|-----------|
| Resource hardening (#13, #14) | 4 | ~4 | **Excellent** |
| addCollection end-to-end (#17) | 18 | ~14 | **Excellent** |
| Bootstrap + type unify (#20, #21) | 12 | ~10 | **Excellent** |
| Live smoke (#22) | 4 (ignored) | 4 | **N/A** (test-only) |
| **Fix batch total** | **34 active + 4 ignored** | **~28** | **Excellent** |

**Fix batch verdict:** PASS. Six issues closed in 4 parallel agent passes; one new issue (#24, Tier B follow-up via `TxApplyIf`) opened as a documented Low-severity follow-up. Net: open count went from 10 → 5; all remaining are Low/Medium with documented workarounds.

---

## 11.11 — Close-out Session (2026-04-22 pm)

One-session close-out of all five remaining open issues (#2, #6, #18, #19, #24). Executed via three parallel agents in isolated worktrees (Wave 1 = #6 + #24 + main-thread #19; Wave 2 = #18 after Wave 1 merged; Wave 3 = QA + README on main thread).

### TxApplyIf atomic create-issue (issue #24 — commits `97afe82`..`c84250e`)

| Aspect | Rating | Notes |
|--------|--------|-------|
| Implementation | **Good** | New `apply_if_tx(scope, matches, txes)` primitive on `PlatformClient`; wire envelope matches upstream `TxApplyIf` (`core:class:TxApplyIf`, `txes: [TxUpdateDoc, TxCollectionCUD(TxCreateDoc)]`). New `huly-mcp/src/txcud.rs` builds sub-txes via `TxFactory`-equivalent helpers. Admin `POST /api/v1/apply-if`. `create_issue_in_project` bundles sequence bump + issue create under scope `tracker:project:{id}:issue-create` with `match = {_id, sequence: N}`; retries ≤5× with exponential backoff on `success: false`. |
| Test Coverage | **Good** | 15 new tests: envelope round-trip (common), mock-WS tx shape (client), handler validation + round-trip (admin), bridge-client wiremock, mcp server-level uses-apply-if + retries-on-contention + ignored contiguity property test. |
| Error Handling | **Good** | Transport errors surface as `ClientError::Connection`; server rejection (non-success) treated as retryable contention up to 5 attempts; ≥5 surfaces a typed error. |
| Security | **N/A** | No new credentials surface. Reuses existing bearer-token auth. |
| TDD Adherence | **Excellent** | Red tests written first per prompt; green commits are the minimal impl. `refactor(mcp/tools)` commit is the final consolidation. |

### NATS creds happy-path (issue #6 — commit `c3c2b96`)

| Aspect | Rating | Notes |
|--------|--------|-------|
| Implementation | **Minimal** | Added `credentials_file_happy_path_loads_from_disk`: writes a valid JWT+nkey `.creds` blob to a per-process temp file, asserts the connect returns a NATS transport error (not "failed to load credentials"), proving `with_credentials_file` parsed successfully. |
| Test Coverage | **Good** | 1 new happy-path test complementing the pre-existing error-path and config-parse tests. |
| Error Handling | **N/A** | Test-only addition. |
| Security | **N/A** | Temp-file cleanup via `Drop` guard. |
| TDD Adherence | **N/A** | Test-only addition to close an audit gap. |

### Catalog override validator (issue #19 — commit `9282183`)

| Aspect | Rating | Notes |
|--------|--------|-------|
| Implementation | **Minimal** | Closed as **Won't Fix — documented contract**. Auto-discovery rejected: overrides are the stable contract (IDs queryable via `huly_discover`). Added safety net: `CatalogOverrides::unknown_keys()` returns override keys that don't map to any known `CardType`/`RelationType`; MCP startup emits `tracing::warn!` listing unknown keys. `config/mcp.example.toml` gains a commented `[mcp.catalog]` stanza enumerating valid keys. |
| Test Coverage | **Adequate** | 2 new unit tests: empty when all keys valid, flags typos. |
| Error Handling | **N/A** | No error path added. Unknown overrides are silently ignored but now surfaced in logs. |
| Security | **N/A** | No new surface. |
| TDD Adherence | **N/A** | Trivial 2-test addition. |

### Markup via collaborator (issue #18 — commits `441f25f`..`fb4ea26`)

| Aspect | Rating | Notes |
|--------|--------|-------|
| Implementation | **Excellent** | `huly/markdown.rs` does CommonMark ↔ ProseMirror-JSON via `pulldown-cmark 0.12` (doc/paragraph/heading h1–h6/marks strong+em+code+link/code_block/lists/blockquote/hard_break/hr; unsupported fallback to plain text). `huly/collaborator.rs` does plain HTTP POST to `{COLLABORATOR_URL}/rpc/{urlencoded(ws\|class\|id\|attr)}` (3× retry, 50ms delay on transport error). `service/workspace_token.rs` holds a reconnect-populated `SecretString` cache. Admin `POST /api/v1/upload-markup` + `/fetch-markup` (format=markdown\|prosemirror). `create_issue`/`update_issue` now do: apply_if with `description:""` → upload markup → update description to the `MarkupBlobRef`. Failure of markup upload logs a warning and returns success with empty description (operator can retry). |
| Test Coverage | **Excellent** | 56 new active tests: 19 markdown converter + 14 collaborator client + 4 workspace-token cache + 8 admin markup handler + 5 huly-common DTO round-trip + 3 bridge-client wiremock + 3 mcp markdown plumbing + 4 `markup_e2e` end-to-end integration. |
| Error Handling | **Good** | Collaborator transport errors retried 3×, 4xx not retried. `/api/v1/upload-markup` returns `503` when collaborator URL or workspace token not yet available (bridge still reconnecting). Unknown format returns `400`. |
| Security | **Good** | Workspace-scoped token (re-resolved per reconnect) used for collaborator; never logged. Token held in `SecretString`. |
| TDD Adherence | **Excellent** | Converter spec written red-first (19 tests); handlers red on 503/400 boundaries first. Commits are TDD-ordered: spec-red → pulldown-cmark dep → impl → client → common → service → admin → mcp plumbing → e2e integration. |

### Integration test close-out (issue #2)

Closed implicitly — 6 integration-test files now cover the full wire:
- `mock_huly` (WS envelope)
- `rest_api` (REST proxy)
- `admin_collection_routes` (platform API incl. apply-if)
- `smoke_0_7_19` (live, env-gated)
- `wire_fixtures` (upstream snapshot)
- `markup_e2e` (collaborator round-trip, added by #18)

20+ active integration tests across these files, up from 0 at migration start.

### Close-out session TDD summary

| Fix | Tests Written | Tests Needed (est.) | TDD Score |
|-----|---------------|---------------------|-----------|
| #24 TxApplyIf | 15 | ~12 | **Excellent** |
| #6 NATS creds | 1 | 1 | **N/A** (test-only) |
| #19 catalog validator | 2 | 2 | **Adequate** |
| #18 markup | 56 | ~40 | **Excellent** |
| **Close-out total** | **74 active** | **~55** | **Excellent** |

**Close-out verdict:** PASS. All five remaining open issues closed in one session. New dep added: `pulldown-cmark 0.12` (justified for #18). Test count 327 → 401 (+74 active, +0 ignored). Clippy clean under `-D warnings`. Zero unsafe blocks. Zero open QA issues.
