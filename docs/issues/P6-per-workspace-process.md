# P6 — One bridge process per workspace

**Phase:** P6 (implements D4).
**Branch:** `feat/per-workspace-bridge`
**Worktree:** `../huly-kube-p6-perws`
**Depends on:** P4 (smaller bridge is easier to multi-instance).
**Blocks:** none.

## Goal

Real failure isolation: a panic in workspace A's WS handler must not affect workspace B. Achieve this via OS-level process isolation, not in-process supervision.

## Scope

### systemd template unit

Replace `systemd/huly-bridge.service` (single instance) with `systemd/huly-bridge@.service` (template):

```ini
[Unit]
Description=Huly bridge for workspace %i
After=network-online.target nats.service
Wants=network-online.target

[Service]
Type=simple
ExecStart=/usr/local/bin/huly-bridge --config /etc/huly-bridge/workspaces.d/%i.toml
Restart=on-failure
RestartSec=2
User=huly-bridge
Group=huly-bridge
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=multi-user.target
```

Enabled per-workspace: `systemctl enable --now huly-bridge@muhasebot.service`.

### Per-workspace config layout

`/etc/huly-bridge/workspaces.d/<workspace>.toml`:
- One file per workspace.
- Same structure as today's `bridge.toml`, with `[huly] workspace = "<this>"` matching the filename.
- The JWT broker config (`[[workspace_credentials]]` from P3) — each per-workspace process knows only its own credentials.

### Bridge code

- `huly-bridge` accepts `--config <path>` (already does). No code change needed for the binary itself; just runs N instances with N configs.
- Health/observability: each instance journals under `huly-bridge@<ws>`, viewable via `journalctl -u huly-bridge@muhasebot`.

### Ansible

- `ansible/files/workspaces/<ws>.toml.example` template.
- `ansible/deploy-bridge.yml` updated:
  - For each workspace declared in `group_vars` or `host_vars`: render config to `/etc/huly-bridge/workspaces.d/<ws>.toml`, then `systemctl enable --now huly-bridge@<ws>`.
  - Old single-instance unit removed and disabled.
- `ansible/group_vars/bridge.yml` gets a `bridge_workspaces:` list.

### Migration runbook (in-PR)

`docs/operations/migration-per-workspace.md`:
1. Stop old `huly-bridge` service.
2. Render new per-workspace configs from existing single config.
3. Move credentials into per-workspace files.
4. Enable per-workspace template instances.
5. Verify: `nats req huly.event.workspace.ready ''` shows each ws independently.
6. Roll back path: re-enable single unit, restore old config.

## Out of scope

- Active/passive failover for a single workspace. Single bridge instance per workspace is acceptable in beta.
- Cross-host bridge sharding. All workspaces run on one host (Riven) for now.

## Acceptance

- [ ] systemd template `huly-bridge@.service` lands in `systemd/`.
- [ ] Old single-instance unit deleted.
- [ ] Ansible plays render per-workspace configs and enable template instances.
- [ ] `docs/operations/migration-per-workspace.md` covers the cutover.
- [ ] Verified on muhasebot: `systemctl status huly-bridge@muhasebot` shows running; killing it does not affect any other workspace's bridge.

## Out

PR `feat(ops): per-workspace bridge processes via systemd template`.
