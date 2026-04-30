# Huly Bridge & MCP Server

A Rust workspace containing two services for integrating with the [Huly.io](https://huly.io) platform:

- **huly-bridge** -- A systemd service that maintains a persistent WebSocket connection to Huly, reimplements the Huly RPC wire protocol, and forwards all platform events to NATS.
- **huly-mcp** -- An MCP (Model Context Protocol) server that discovers bridge instances via NATS and exposes Huly operations as tools for Claude Code.
ye
## Architecture

```
  Huly.io Platform
    REST + WebSocket
         |
  ┌──────▼──────────────────────────┐
  │      huly-bridge (Rust)         │  (one instance per workspace)
  │                                 │
  │  huly/     Huly API client      │
  │  bridge/   Event loop + proxy   │
  │  admin/    axum :9090           │
  │  service/  systemd lifecycle    │
  └──────┬──────────────────────────┘
         |
    NATS Server
         |
  ┌──────▼──────────────────────────┐
  │      huly-mcp (Rust)            │  (aggregates all workspaces)
  │                                 │
  │  discovery/  NATS announcements │
  │  mcp/        MCP tools (stdio)  │
  └──────┬──────────────────────────┘
         |
    Claude Code (stdio)
```

## Platform Support

| Platform | Build | Runtime | Status |
|----------|-------|---------|--------|
| Linux x86_64 (glibc) | `make linux` | systemd | **Primary target** — developed and tested here |
| Windows x86_64 (MinGW) | `make windows` | Windows Service (e.g. NSSM) | **Awaiting QA validation** — cross-compiled, not tested by the dev team |

Windows binaries are produced via [`cross`](https://github.com/cross-rs/cross) with Podman. See [`DEPLOY.md`](DEPLOY.md) for the full Windows deployment procedure and the QA checklist.

## Quick Start

```bash
# Build all crates (host target)
cargo build --release

# Or build for both platforms at once
make all        # linux + windows
make linux      # linux only
make windows    # windows only (requires cross + podman)
make release    # copies binaries into dist/{linux,windows}-x86_64/

# Run the bridge (one per workspace)
./target/release/huly-bridge --config config/bridge.example.toml

# Run the MCP server (connects to NATS, discovers all bridges)
./target/release/huly-mcp --config config/mcp.example.toml
```

### Claude Code Integration

Add the MCP server to your `.mcp.json`:

```json
{
  "mcpServers": {
    "huly": {
      "command": "/path/to/huly-mcp",
      "args": ["--config", "/path/to/mcp.toml"]
    }
  }
}
```

**Available MCP tools:**

Generic escape hatches (work with any class):

| Tool | Description |
|------|-------------|
| `huly_list_workspaces` | List all discovered workspaces and their status |
| `huly_status` | Get bridge health for a specific or all workspaces |
| `huly_find` | Find documents by class and query filter |
| `huly_get` | Get a single document |
| `huly_create` | Create a new document |
| `huly_update` | Update an existing document |
| `huly_delete` | Delete a document |

Domain-specific (Tracker + cards, mirrors upstream `@hcengineering/mcp-server`):

| Tool | Description |
|------|-------------|
| `huly_discover` | List projects, components, statuses, card types, associations in a workspace |
| `huly_find_cards` | Search spec cards by type (Module Spec, Data Entity, …) with title filter |
| `huly_find_issues` | Search Tracker issues by component / status / title |
| `huly_get_issue` | Get an issue by identifier (e.g. `MUH-3`) plus its linked relations |
| `huly_create_issue` | Create a Tracker issue (auto-bumps project sequence) |
| `huly_update_issue` | Sparse update of an issue by identifier |
| `huly_create_component` | Create a Tracker component (idempotent on label) |
| `huly_link_issue_to_card` | Create a `core:class:Relation` between an issue and a card |
| `huly_create_project` | Create a Tracker project from a local README.md |

Markup (rich text via the Huly collaborator service):

| Tool | Description |
|------|-------------|
| `huly_upload_markup` | Convert markdown → ProseMirror and upload for an object attribute; returns the `MarkupBlobRef` |
| `huly_fetch_markup` | Fetch markup as `markdown` (lossy) or `prosemirror` (lossless) for an object attribute |

Sync pipeline (shells out to `huly-api/packages/sync`):

| Tool | Description |
|------|-------------|
| `huly_sync_status` | Compare local `docs/` against `.huly-sync-state.json`; report new/modified/deleted |
| `huly_sync_cards` | Run the card sync pipeline (Enums → MasterTags → Associations → Cards → Binaries → Relations); supports `dry_run` |

## Configuration

Configuration is via a TOML file. See [`config/bridge.example.toml`](config/bridge.example.toml) for a full example.

### `[huly]` -- Huly Platform Connection

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `url` | string | **required** | Huly server URL (e.g. `https://huly.example.com`) |
| `workspace` | string | **required** | Workspace identifier |
| `use_binary_protocol` | bool | `true` | Use MessagePack instead of JSON for wire format |
| `use_compression` | bool | `true` | Use Snappy compression on the wire |
| `reconnect_delay_ms` | u64 | `1000` | Delay between reconnection attempts (ms) |
| `ping_interval_secs` | u64 | `10` | WebSocket ping interval (seconds) |

### `[huly.auth]` -- Authentication

**Token-based** (static API token):

```toml
[huly.auth]
method = "token"
token = "your-api-token"
```

**Password-based** (email/password login):

```toml
[huly.auth]
method = "password"
email = "user@example.com"
password = "your-password"
```

Password authentication calls `POST {url}/api/v1/accounts/login` and receives a session token.

The `AccountsClient` also exposes OTP login (`loginOtp`, `validateOtp`) via the same JSON-RPC endpoint for flows that prefer one-time codes.

### `[nats]` -- NATS Message Queue

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `url` | string | `nats://127.0.0.1:4222` | NATS server URL |
| `subject_prefix` | string | `huly` | Prefix for NATS subjects |
| `credentials` | string | *none* | Path to NATS credentials file |

### `[admin]` -- Admin API Server

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `host` | string | `127.0.0.1` | Bind address |
| `port` | u16 | `9090` | Bind port |

### `[log]` -- Logging

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `level` | string | `info` | Log level (`debug`, `info`, `warn`, `error`) |
| `json` | bool | `false` | Enable JSON log format |

---

## Supported Actions

### Platform Client (RPC Methods)

The bridge reimplements the Huly RPC protocol over WebSocket and exposes the following operations via the `PlatformClient` trait:

#### `findAll`

Query multiple documents by class and filter.

| Parameter | Type | Description |
|-----------|------|-------------|
| `class` | string | Document class (e.g. `core:class:Issue`) |
| `query` | object | Query filter |
| `options` | FindOptions | Optional: `limit`, `sort`, `lookup`, `projection` |

**Returns:** `FindResult { docs: [Doc], total: u64, lookup_map?: object }`

**Example query:**
```json
["core:class:Issue", {"space": "project-1"}, {"limit": 10, "sort": {"modifiedOn": -1}}]
```

#### `findOne`

Find a single document. Internally calls `findAll` with `limit: 1`.

| Parameter | Type | Description |
|-----------|------|-------------|
| `class` | string | Document class |
| `query` | object | Query filter |
| `options` | FindOptions | Optional query options |

**Returns:** `Option<Doc>` -- the first matching document or null.

#### `createDoc`

Create a new document in a space.

| Parameter | Type | Description |
|-----------|------|-------------|
| `class` | string | Document class |
| `space` | string | Target space/container ID |
| `attributes` | object | Document fields |

**Returns:** `string` -- the ID of the created document.

**Example:**
```json
["core:class:Issue", "project-1", {"title": "Fix bug", "priority": 1}]
```

#### `updateDoc`

Update an existing document.

| Parameter | Type | Description |
|-----------|------|-------------|
| `class` | string | Document class |
| `space` | string | Space ID |
| `id` | string | Document ID |
| `operations` | object | Update operations |

**Returns:** `TxResult { success: bool, id?: string }`

**Example:**
```json
["core:class:Issue", "project-1", "issue-42", {"title": "Updated title"}]
```

#### `removeDoc`

Delete a document.

| Parameter | Type | Description |
|-----------|------|-------------|
| `class` | string | Document class |
| `space` | string | Space ID |
| `id` | string | Document ID |

**Returns:** `TxResult { success: bool }`

### Query Options

All `find*` methods accept optional `FindOptions`:

```json
{
  "limit": 50,
  "sort": {"modifiedOn": -1},
  "lookup": {"_id": {"assignee": "core:class:Employee"}},
  "projection": {"title": 1, "status": 1}
}
```

### Document Structure

Every document returned from Huly has these standard fields:

```json
{
  "_id": "document-id",
  "_class": "core:class:Issue",
  "space": "project-1",
  "modifiedOn": 1700000000000,
  "modifiedBy": "user:john",
  "title": "...",
  "...": "additional fields"
}
```

---

## Events and NATS Forwarding

The bridge listens for server-pushed events on the WebSocket connection (messages with `id == -1`) and forwards them to NATS.

### Subject Mapping

Events are published to NATS subjects using the pattern:

```
{subject_prefix}.events.{event_type}
```

The `event_type` is extracted from the `event` field in the push payload. If no `event` field is present, it defaults to `unknown`.

**Examples:**

| Huly Push Payload | NATS Subject |
|-------------------|-------------|
| `{"event": "tx", "data": {...}}` | `huly.events.tx` |
| `{"event": "notification", ...}` | `huly.events.notification` |
| `{"event": "doc.created", ...}` | `huly.events.doc.created` |
| `{"data": "no event field"}` | `huly.events.unknown` |

### Event Payload

Events are published to NATS as JSON-serialized bytes of the `result` field from the server push.

### Event Loop Statistics

The event loop tracks:

| Stat | Description |
|------|-------------|
| `events_forwarded` | Successfully published to NATS |
| `events_failed` | Failed to publish (NATS errors) |

---

## REST Proxy

The bridge includes a REST proxy that forwards HTTP requests to the Huly REST API with automatic authentication.

### Supported HTTP Methods

- `GET`
- `POST`
- `PUT`
- `DELETE`
- `PATCH`

All proxied requests include the `Authorization: Bearer {token}` header automatically.

### Error Handling

| Error | Description |
|-------|-------------|
| `Network` | Huly server unreachable |
| `Upstream { status, body }` | Non-2xx response from Huly |
| `Format` | Response is not valid JSON |
| `UnsupportedMethod` | HTTP method not in the supported list |

### Rate-limit metadata

REST responses are wrapped in `ProxyResponse { body, rate_limit }`. The `RateLimitInfo` extractor parses these headers (all optional):

| Header | Field |
|--------|-------|
| `X-RateLimit-Limit` | `limit` |
| `X-RateLimit-Remaining` | `remaining` |
| `X-RateLimit-Reset` | `reset_ms` |
| `Retry-After-ms` | `retry_after_ms` |
| `Retry-After` (seconds) | `retry_after_ms` (× 1000, fallback) |

### Typed REST client (`RestClient`)

In addition to the generic proxy, `huly-bridge` exposes a typed `RestClient` for the Huly 0.7.19 REST surface:

| Method | Endpoint |
|--------|----------|
| `get_config()` | `GET /config.json` (bootstrap; ACCOUNTS_URL, COLLABORATOR_URL, FILES_URL, UPLOAD_URL) |
| `get_account(workspace)` | `GET /api/v1/account/{workspace}` |
| `get_model(workspace, full)` | `GET /api/v1/load-model/{workspace}?full=…` |
| `search_fulltext(workspace, query, opts)` | `GET /api/v1/search-fulltext/{workspace}` |
| `domain_request(workspace, domain, params)` | `POST /api/v1/request/{domain}/{workspace}` |
| `ensure_person(workspace, …)` | `POST /api/v1/ensure-person/{workspace}` |

All methods return `(T, RateLimitInfo)`, transparently snappy-decode `Content-Encoding: snappy` responses, and surface 429s as `RestError::RateLimited` with parsed `RateLimitInfo`.

---

## Admin API Endpoints

The admin API server listens on the configured host/port (default `127.0.0.1:9090`).

### `GET /healthz` -- Liveness Probe

Always returns `200 OK` if the process is running. Suitable for Kubernetes liveness probes or systemd health checks.

### `GET /readyz` -- Readiness Probe

| Status | Condition |
|--------|-----------|
| `200 OK` | Both Huly WebSocket **and** NATS are connected |
| `503 Service Unavailable` | Either connection is down |

### `GET /metrics` -- Prometheus Metrics

Returns metrics in Prometheus text exposition format.

**Available metrics:**

| Metric | Type | Description |
|--------|------|-------------|
| `huly_bridge_events_forwarded_total` | counter | Total events successfully published to NATS |
| `huly_bridge_events_failed_total` | counter | Total events that failed to publish |
| `huly_bridge_events_dropped_total` | counter | Total server-push events dropped (event channel full or closed) |
| `huly_bridge_ws_reconnects_total` | counter | Total WebSocket reconnection attempts |
| `huly_bridge_pending_requests_dropped_total` | counter | Total RPC requests rejected because the in-flight pending map hit its cap |
| `huly_bridge_ws_connected` | gauge | WebSocket connection status (`0` or `1`) |
| `huly_bridge_nats_connected` | gauge | NATS connection status (`0` or `1`) |

**Example output:**
```
# TYPE huly_bridge_events_forwarded_total counter
huly_bridge_events_forwarded_total 42

# TYPE huly_bridge_ws_connected gauge
huly_bridge_ws_connected 1

# TYPE huly_bridge_nats_connected gauge
huly_bridge_nats_connected 1
```

### `GET /api/v1/status` -- JSON Status

Returns a JSON object with current service state:

```json
{
  "uptime_secs": 3600,
  "huly_connected": true,
  "nats_connected": true,
  "ready": true
}
```

### Platform API (Bearer-authenticated)

When a `PlatformClient` is wired (Huly WS connection up) the admin server also
exposes a JSON-over-HTTP surface that mirrors the upstream RPC operations:

| Method | Path | Body | Returns |
|--------|------|------|---------|
| POST | `/api/v1/find` | `{ class, query, options? }` | `FindResult` |
| POST | `/api/v1/find-one` | `{ class, query, options? }` | `Doc \| null` |
| POST | `/api/v1/create` | `{ class, space, attributes }` | `{ id }` |
| POST | `/api/v1/update` | `{ class, space, id, operations }` | `TxResult` |
| POST | `/api/v1/delete` | `{ class, space, id }` | `TxResult` |
| POST | `/api/v1/add-collection` | `{ class, space, attachedTo, attachedToClass, collection, attributes }` | `{ id }` |
| POST | `/api/v1/update-collection` | `{ class, space, id, attachedTo, attachedToClass, collection, operations }` | `TxResult` |
| POST | `/api/v1/apply-if` | `{ scope, matches: [{_class, query}], txes: [TxCUD] }` | `{ success, serverTime }` |
| POST | `/api/v1/upload-markup` | `{ objectClass, objectId, objectAttr, markdown }` | `{ ref }` |
| POST | `/api/v1/fetch-markup` | `{ objectClass, objectId, objectAttr, sourceRef?, format? }` | `{ content, format }` |

`update` accepts arbitrary operators in `operations` (notably `$inc` for
server-atomic counter bumps such as `tracker:class:Project.sequence`).

`add-collection` / `update-collection` mirror the upstream `addCollection` /
`updateCollection` wrappers used by the tracker for parent-attached docs
(e.g. issues attached to a project under the `subIssues` collection).

`apply-if` mirrors upstream `TxApplyIf`: executes `txes` only if every query
in `matches` returns ≥1 document. Same `scope` string is serialised
server-side, so concurrent callers with the same scope queue behind each
other. Used by `huly_create_issue` to atomically bundle the project
sequence bump with the new-issue `TxCollectionCUD` — this closes both the
identifier uniqueness and contiguity guarantees.

`upload-markup` / `fetch-markup` talk to Huly's collaborator service. The
bridge accepts markdown input, converts it to ProseMirror JSON internally,
calls the collaborator's `createContent` / `getContent` RPCs, and returns
a `MarkupBlobRef` (a `Ref<Blob>` string) suitable for writing back to
document attributes of type `MarkupBlobRef` — e.g. `Issue.description`.
Returns `503` while the bridge is still reconnecting (workspace token or
`COLLABORATOR_URL` not yet resolved).

---

## Wire Protocol

The bridge reimplements the Huly RPC protocol used by the official TypeScript client (`@hcengineering/api-client`).

### Protocol Negotiation

On WebSocket connect, a `#hello` handshake negotiates the wire format:

```json
// Client -> Server
{"method": "#hello", "params": [], "id": 1, "binary": true, "compression": true}

// Server -> Client
{"id": 1, "binary": true, "compression": true}
```

### Serialization Modes

| Mode | binary | compression | Description |
|------|--------|-------------|-------------|
| JSON | `false` | `false` | Plain JSON text messages |
| JSON + Snappy | `false` | `true` | Snappy-compressed JSON |
| MessagePack | `true` | `false` | MessagePack binary messages |
| MessagePack + Snappy | `true` | `true` | Snappy-compressed MessagePack **(default)** |

### Request Format

```json
{
  "id": 1,
  "method": "findAll",
  "params": ["core:class:Issue", {"space": "s1"}],
  "meta": null,
  "time": 1700000000000
}
```

### Response Format

```json
{
  "id": 1,
  "result": {"docs": [...], "total": 5},
  "error": null,
  "chunk": null,
  "rateLimit": null,
  "terminate": null
}
```

### Server Push Events

Server-initiated messages use `id: -1`:

```json
{"id": -1, "result": {"event": "tx", "data": {...}}}
```

### Rate Limiting

If the server returns a `rateLimit` field, the response includes a `retryAfter` value in milliseconds:

```json
{"id": 3, "result": null, "rateLimit": {"retryAfter": 5000}}
```

---

## Error Classification

Errors are classified into two categories to determine retry behavior:

| Category | Error Types | Behavior |
|----------|-------------|----------|
| **Transient** | `ConnectionLost`, `NatsPublish` | Retry with backoff |
| **Fatal** | `AuthFailed`, `Config` | Shutdown service |

---

## systemd Integration

### Unit File

See [`systemd/huly-bridge@.service`](systemd/huly-bridge@.service) for the full unit file. It's a template — one instance per workspace, spawned as `huly-bridge@<workspace>.service`.

**Key settings:**

```ini
Type=simple
Restart=on-failure
RestartSec=2s
DynamicUser=yes      # Security hardening (per-instance dynamic user)
ProtectSystem=strict
ReadOnlyPaths=/etc/huly-bridge
NoNewPrivileges=yes
MemoryMax=512M
```

### Startup Sequence

1. Load and parse TOML configuration
2. Initialize tracing (journald or stdout)
3. Initialize Prometheus metrics
4. Authenticate with Huly (token or password)
5. Connect to NATS
6. Connect to Huly WebSocket + perform hello handshake
7. Send `READY=1` to systemd
8. Start admin API server
9. Start watchdog pinger (every 10s)
10. Start event forwarding loop

### Shutdown

On `SIGTERM` or `SIGINT`:

1. Send `STOPPING=1` to systemd
2. Cancel all tasks via `CancellationToken`
3. Wait up to 10s for event loop to drain
4. Log final statistics and exit

### Watchdog

The watchdog pinger runs every 10 seconds:

- If both Huly and NATS are connected: sends `WATCHDOG=1` to systemd
- If either is disconnected: **skips the ping** (systemd will restart the service after `WatchdogSec` timeout)

### Installation

```bash
# Build
cargo build --release

# Install binary
sudo cp target/release/huly-bridge /usr/local/bin/

# Install config (one file per workspace; filename = workspace name)
sudo install -d -m 0755 /etc/huly-bridge/workspaces.d
sudo cp config/workspaces.d/muhasebot.toml.example \
        /etc/huly-bridge/workspaces.d/muhasebot.toml
# Edit /etc/huly-bridge/workspaces.d/muhasebot.toml with your settings

# Install systemd template unit (one template, N instances)
sudo cp systemd/huly-bridge@.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now huly-bridge@muhasebot.service

# Check status
sudo systemctl status huly-bridge@muhasebot.service
sudo journalctl -u huly-bridge@muhasebot.service -f
```

See [`docs/operations/migration-per-workspace.md`](docs/operations/migration-per-workspace.md) when upgrading from a pre-P6 single-instance bridge.

---

## Project Structure

```
huly-kube/
  Cargo.toml                     # Workspace manifest
  crates/
    huly-common/                 # Shared types (Doc, FindResult, announcements, API types)
      src/lib.rs
    huly-bridge/                 # Bridge service
      src/
        lib.rs                   # Library re-exports (enables integration tests)
        main.rs                  # Entry point
        config.rs                # TOML config structs
        error.rs                 # Error classification
        huly/                    # Huly API client (auth, rpc, rest, ws, client)
        bridge/                  # Event loop, NATS publisher, REST proxy, rate-limit, announcer
        admin/                   # axum routes, health, metrics, platform API
        service/                 # Lifecycle, watchdog
      tests/
        common/                  # Fixture loader, ephemeral_port/nats helpers
        mock_huly.rs             # In-process mock Huly server (REST + WS) + WsScript builder
        rest_api.rs              # MockHuly-driven REST integration tests
        wire_fixtures.rs         # Fixture-shape sanity tests
        fixtures/                # 0.7.19-shaped JSON wire samples
    huly-mcp/                    # MCP server
      src/
        main.rs                  # Entry point (stdio MCP server)
        config.rs                # MCP config (incl. [mcp.sync], [mcp.catalog])
        discovery.rs             # NATS-based bridge discovery
        bridge_client.rs         # HTTP client for bridge REST API
        sync.rs                  # SyncRunner subprocess wrapper for huly-api/packages/sync
        mcp/server.rs            # MCP tool registration (20 tools)
        mcp/tools.rs             # Tool implementations (find/get/create/update/delete/discover/cards/issues/components/links/projects)
        mcp/catalog.rs           # Card-type / status / relation enum ↔ ID maps (overridable)
      tests/fixtures/            # Stub sync subprocess scripts
  config/
    bridge.example.toml          # legacy single-instance shape (reference)
    mcp.example.toml
    workspaces.d/
      muhasebot.toml.example     # per-workspace bridge config (P6 layout)
  systemd/
    huly-bridge@.service         # template unit; one instance per workspace
```

## Testing

```bash
# Run all tests across the workspace
cargo test --workspace

# Run specific crate tests
cargo test -p huly-bridge
cargo test -p huly-common
cargo test -p huly-mcp

# Lint
cargo clippy --workspace
```

The workspace test suite runs on Linux only. Windows runtime validation is handled by QA — see the Windows section in [`DEPLOY.md`](DEPLOY.md#10-windows-deployment-awaiting-qa-validation).

## License

See LICENSE file for details.
