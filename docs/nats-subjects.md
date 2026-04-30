# NATS subject taxonomy

Single source of truth for the NATS subjects this workspace publishes
and the wire shape of each payload. Two top-level namespaces:

- **`huly.event.*`** — what the upstream Huly transactor reports.
  Published by `huly-bridge`. The canonical "what actually happened"
  stream.
- **`huly.mcp.*`** — what the MCP server tried, and the result it
  observed. Published by `huly-mcp`. The complementary "what the AI
  attempted" stream.

A third surface, `huly.bridge.mint`, is the JWT broker request/reply
channel; it is documented in `huly-common::mint` and is **not** an
audit/event stream.

Subscribers correlate `huly.event.tx.*` and `huly.mcp.*` events on
`request_id` (a ULID stamped by MCP at tool entry; see [`huly-mcp`
audit module](#correlation)).

---

## 1. Bridge: `huly.event.*`

Published by `huly-bridge`. Subject prefix is configurable
(`[nats] subject_prefix`); default `huly`. All payloads are JSON.

| Subject | Trigger | Payload shape (JSON) |
|---|---|---|
| `huly.event.tx` | Every transactor TX (CRUD/Mixin/ApplyIf) | `{ event: "tx", tx: <Tx> }` |
| `huly.event.notification` | Notification-class events | `{ event: "notification", … }` |
| `huly.event.<other>` | Any other upstream event class | `{ event: "<other>", … }` |

`<Tx>` is the upstream `core:class:Tx*` shape. When the TX originated
from an MCP tool call, `<Tx>.meta.request_id` carries the audit
correlator.

**Stability.** `huly.event.*` subjects are **stable** in v2 beta —
new event classes may be added; existing classes will not be renamed
or removed.

---

## 2. MCP audit: `huly.mcp.*`

Published by `huly-mcp` from every `#[tool]` invocation. All payloads
are JSON.

### 2.1 `huly.mcp.tool.invoked`

Emitted at tool entry, before any side effect.

```jsonc
{
  "tool":          "huly_create_issue",
  "workspace":     "ws-1",                     // optional
  "agent_id":      "ai-cli-001",
  "params_digest": "deadbeefcafebabe",         // SHA-256(body), 16 hex chars
  "request_id":    "01J0RID000000000000000000",// ULID
  "timestamp_ms":  1735000000000
}
```

### 2.2 `huly.mcp.action.<class>.<op>`

Emitted **only for mutating tools**, immediately after `tool.invoked`.
`<class>` is the dotted Huly class name with the `:class:` infix
dropped (`tracker:class:Issue` → `tracker.issue`); `<op>` is the verb
(`create`, `update`, `delete`, `link`).

```jsonc
{
  "workspace":     "ws-1",
  "agent_id":      "ai-cli-001",
  "request_id":    "01J0RID000000000000000000",
  "target_id":     "iss-42",                  // optional; absent on create
  "fields_changed":["title", "priority"],     // optional; absent on create/delete
  "timestamp_ms":  1735000000000
}
```

Examples:
- `huly.mcp.action.tracker.issue.create`
- `huly.mcp.action.tracker.issue.update`
- `huly.mcp.action.tracker.issue.delete`
- `huly.mcp.action.tracker.issue.link`
- `huly.mcp.action.tracker.component.create`
- `huly.mcp.action.tracker.project.create`
- `huly.mcp.action.card.create`
- `huly.mcp.action.<plugin>.<class>.<create|update|delete>` (generic
  `huly_create` / `huly_update` / `huly_delete` mutators).

### 2.3 `huly.mcp.tool.completed`

Emitted at tool exit, success or failure. The `result` discriminator
is `"ok"` or `"err"`.

```jsonc
// success
{
  "request_id":    "01J0RID000000000000000000",
  "tool":          "huly_create_issue",
  "result":        "ok",
  "result_digest": "1234567890abcdef",
  "duration_ms":   42,
  "timestamp_ms":  1735000000042
}

// failure
{
  "request_id":   "01J0RID000000000000000000",
  "tool":         "huly_create_issue",
  "result":       "err",
  "error":        "rpc error: code=403, message=denied",
  "duration_ms":  17,
  "timestamp_ms": 1735000000017
}
```

### 2.4 `huly.mcp.error`

Emitted alongside `tool.completed` when the tool returned `Err(_)` and
a structured transactor `Status` could be decoded.

```jsonc
{
  "request_id":            "01J0RID000000000000000000",
  "tool":                  "huly_create_issue",
  "code":                  "platform:status:Forbidden",
  "message":               "denied: workspace membership required",
  "params":                {"reason": "scope"},
  "transactor_request_id": null,
  "timestamp_ms":           1735000000017
}
```

### 2.5 Digesting

`params_digest` and `result_digest` are **always**
`SHA-256(json_body) → hex → first 16 chars` (8 bytes / 64 bits). The
truncation is collision-tolerant for typical audit traffic and avoids
putting raw bodies on the wire.

### 2.6 Correlation

Every mutating tool call is uniquely identified by its **`request_id`**
(a [ULID](https://github.com/ulid/spec) minted at tool entry). The id
appears:

1. In every `huly.mcp.*` event for that call.
2. In `huly.event.tx.*` payloads' `tx.meta.request_id`, because MCP
   wraps the tool body in a `with_request_id` task scope and the
   REST client stamps the id onto each TX envelope before sending.

A subscriber that joins MCP intent to transactor outcome can do so on
this single id without inspecting payload bodies.

### Stability

`huly.mcp.*` subjects + the documented field set are **stable** in
v2 beta. New optional fields may be added; existing fields will not be
removed or renamed without a major version bump. Constants live in
[`huly_common::mcp_subjects`](../crates/huly-common/src/mcp_subjects.rs).

---

## 3. Out of scope

- **JetStream retention.** The audit channel is plain NATS publish at
  v2 beta. JetStream retention / replay is a future ratify (D7).
- **Subject ACLs.** Operator concern — apply NATS account permissions
  if you need to restrict which agents can publish under
  `huly.mcp.*`. The bridge publishes only `huly.event.*` and replies
  on `huly.bridge.mint.*`; MCP publishes only `huly.mcp.*`.
- **Bidirectional control.** Killing in-flight tool calls / agents is
  not part of this surface; future work.
