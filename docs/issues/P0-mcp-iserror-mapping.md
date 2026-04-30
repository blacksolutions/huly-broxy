# P0 — MCP isError mapping

**Phase:** P0 (deferred fix). Independent of v2; lands first.
**Branch:** `fix/mcp-iserror-mapping`
**Worktree:** `../huly-kube-p0-iserror`
**Depends on:** none.
**Blocks:** nothing strictly, but reduces noise during downstream PR reviews.

## Problem

Every `#[tool]` method in `crates/huly-mcp/src/mcp/server.rs` returns `String`. Both success bodies and `"Error: ..."` text go into the same string, mapped by `rmcp 1.3` to `CallToolResult::success` (`isError: None`). Clients that trust `isError` are misled — failures look like successes.

Reference: `rmcp-1.3.0/src/handler/server/tool.rs:88-95` — `Result<T, E>` where both arms implement `IntoContents` is mapped to `CallToolResult::error` on `Err`, setting `isError: true`.

## Scope

Flip every `#[tool]` method's return type from `-> String` to `-> Result<String, String>`. ~20 methods in `crates/huly-mcp/src/mcp/server.rs`.

Touch points:
- All `#[tool]` methods (search for `#[tool(`).
- Helper methods that return early-error strings (`resolve_proxy_url`, `resolve_optional_workspace`) should change to `Result` too — currently they return `String` for the early-return-error case, which now needs to be `Err`.
- Tests that assert `result.starts_with("Error:")` (`server.rs:1708,1815`) → assert `result.is_err()`.

Pattern:
```rust
match self.http_client.delete(...).await {
    Ok(r)  => Ok(serde_json::to_string_pretty(&r).map_err(|e| format!("serialize: {e}"))?),
    Err(e) => Err(format!("{e}")),
}
```

## Out of scope

- Restructuring error payloads into JSON (`{error, code}` shape). Keep raw strings; downstream consumers can grep.
- Changing public tool descriptions or input schemas.

## Acceptance

- [ ] Every `#[tool]` method returns `Result<String, String>` (or compatible `Result<T, E>` where both implement `IntoContents`).
- [ ] `make clippy` clean.
- [ ] `make test` green; updated assertions reflect `Err` paths.
- [ ] One smoke: build + run huly-mcp against a known-failing call (e.g., bad workspace), verify the JSON-RPC response has `"isError": true`.

## Out

A PR titled `fix(huly-mcp): return Result so isError surfaces correctly`. Single commit, GPG-signed, no Co-Authored-By.
