# CLAUDE.md


## Project
Huly.io Bridge Server. Rust systemd service. Wraps huly.io API client, proxies upstream API, forwards events internally over NATS.
Workspace crates: `huly-common`, `huly-bridge`, `huly-mcp`. Edition 2024.

## Rules
- Use TDD for new behavior and bug fixes; skip for typos, renames, comments, dep bumps.
- Run `make clippy` and `make test` before reporting code complete.
- Reports: extremely concise, sacrifice grammar for concision.
- Use a worktree (`git worktree add ../huly-kube-<topic>`) for changes touching >1 crate or risky refactors.
- Use agents for >3-query searches or independent parallel work; skip for known paths.
- Run independent agents in parallel to save wall-time and isolate context.
- Conventional commits. Scopes: `huly-common`, `huly-bridge`, `huly-mcp`, `qa`, `apply-if`, `merge`, `docs`.
- Push back on vague asks. One question at a time, not three.
- Surface implications, don't bury them.

## Build / test / lint
- Build Linux: `make linux`. Build Windows: `make windows`. Both: `make release`.
- Test: `make test`. Lint: `make clippy`.

## Config / secrets
- Runtime config: `config/bridge.toml`, `config/mcp.toml`. Templates: `*.example.toml`. Real configs are gitignored — never commit secrets.

## Deploy gate
- Windows builds are cross-compiled but QA-gated. Dev cannot sign off Windows runtime; QA owns it.

## Branch / PR policy
- See `branch-pr-policy` skill for branch naming, merge strategy, reviewer policy.
