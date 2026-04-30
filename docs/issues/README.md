# v2 Refactor Issues

Phases of the v2 architecture refactor. See [`../architecture/v2-direct-mcp.md`](../architecture/v2-direct-mcp.md) for the design rationale.

## Phase order + dependency graph

```
P0 ─────────────┐                            (independent)
P1 ─────────────┤── (decides D1, transport)
P2 ─────────────┘                            (independent, mechanical)
                │
                ▼
P3 ──────── depends on P2
                │
                ▼
P4 ──────── depends on P1, P2, P3
                │
        ┌───────┴───────┐
        ▼               ▼
       P6              P7
```

| Phase | File | Status | Owner |
|---|---|---|---|
| P0 | [P0-mcp-iserror-mapping.md](P0-mcp-iserror-mapping.md) | open | TBA |
| P1 | [P1-rest-spike.md](P1-rest-spike.md) | open | TBA |
| P2 | [P2-hoist-huly-client.md](P2-hoist-huly-client.md) | open | TBA |
| P3 | [P3-jwt-broker.md](P3-jwt-broker.md) | open | TBA |
| P4 | [P4-mcp-direct-and-cleanup.md](P4-mcp-direct-and-cleanup.md) | open | TBA |
| P5 | [P5-merged-into-P4.md](P5-merged-into-P4.md) | tombstone | — |
| P6 | [P6-per-workspace-process.md](P6-per-workspace-process.md) | open | TBA |
| P7 | [P7-mcp-audit-publishes.md](P7-mcp-audit-publishes.md) | open | TBA |

## Workflow

Each phase has a worktree branch (`../huly-kube-p<n>-*`), a target branch name, and acceptance criteria. Agents consume one issue file at a time, work in the worktree, commit GPG-signed conventional-commit messages with no Co-Authored-By trailer, and report back with the PR URL. Local merges to `main` are coordinated by the owner.
