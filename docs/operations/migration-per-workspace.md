# Migration: per-workspace bridge processes (P6)

Cutover from a single `huly-bridge.service` instance handling every
workspace to one `huly-bridge@<ws>.service` per workspace. OS-level
isolation: a panic in workspace A's WS handler can no longer take
workspace B down.

## Contents

1. [Pre-flight](#1-pre-flight)
2. [Stop the old service](#2-stop-the-old-service)
3. [Render per-workspace configs](#3-render-per-workspace-configs)
4. [Move credentials per workspace](#4-move-credentials-per-workspace)
5. [Install the template unit and enable instances](#5-install-the-template-unit-and-enable-instances)
6. [Verify each workspace](#6-verify-each-workspace)
7. [Remove the legacy artifacts](#7-remove-the-legacy-artifacts)
8. [Roll back](#8-roll-back)

---

## 1. Pre-flight

On the bridge host (Riven for current beta):

```bash
# Capture the running config + service name for rollback.
sudo cp /etc/huly-bridge/bridge.toml /root/huly-bridge.toml.preP6
sudo systemctl status huly-bridge.service > /root/huly-bridge.preP6.status
```

Enumerate the workspaces this host should serve. For Riven beta this is
just `muhasebot`. If the legacy single instance was multiplexing more
than one workspace via JWT broker config, list every entry from
`[[workspace_credentials]]`.

## 2. Stop the old service

```bash
sudo systemctl stop huly-bridge.service
sudo systemctl disable huly-bridge.service
```

The bridge is now down. Plan for ~minutes of downtime, not seconds —
the cutover renders configs, copies credentials, then enables N new
instances.

## 3. Render per-workspace configs

```bash
sudo install -d -m 0755 /etc/huly-bridge/workspaces.d
```

For each workspace `<ws>`, copy the legacy config and adjust three
fields: `[huly] workspace`, the auth block (token or password matching
that workspace), and `[admin] port` (must be unique per instance on the
host).

```bash
sudo cp /etc/huly-bridge/bridge.toml \
        /etc/huly-bridge/workspaces.d/muhasebot.toml
sudo $EDITOR /etc/huly-bridge/workspaces.d/muhasebot.toml
sudo chmod 0640 /etc/huly-bridge/workspaces.d/muhasebot.toml
sudo chown root:root /etc/huly-bridge/workspaces.d/muhasebot.toml
```

If you use the Ansible play, drop a vault-encrypted
`ansible/files/workspaces/<ws>.toml` per workspace and re-run
`make deploy` instead of editing on the host.

## 4. Move credentials per workspace

If the legacy config used `[[workspace_credentials]]` (P3 JWT broker)
to hold credentials for multiple workspaces, split them: each
per-workspace TOML carries only its own credentials in `[huly.auth]`
(or its own `[[workspace_credentials]]` block restricted to that one
workspace).

Reasoning: a per-workspace process should never hold another
workspace's secret. Keeps blast radius matched to the process boundary.

## 5. Install the template unit and enable instances

```bash
sudo install -m 0644 systemd/huly-bridge@.service \
                     /etc/systemd/system/huly-bridge@.service
sudo systemctl daemon-reload

# Enable + start one instance per workspace.
sudo systemctl enable --now huly-bridge@muhasebot.service
# repeat per workspace
```

Ansible workflow does these steps automatically once
`bridge_workspaces` is populated.

## 6. Verify each workspace

```bash
# Per-instance status + logs.
sudo systemctl status huly-bridge@muhasebot.service
journalctl -u huly-bridge@muhasebot.service -n 100 --no-pager

# Health endpoint (port from that workspace's [admin].port).
curl -s http://127.0.0.1:9095/readyz
```

NATS-side verification — each workspace announces independently:

```bash
nats sub 'huly.bridge.announce'        # one announce per workspace
nats sub 'huly.event.>'                # event traffic, scoped per ws subject
```

Failure-isolation check (run only in a maintenance window):

```bash
# Kill one workspace's bridge; confirm the other instances stay up.
sudo systemctl kill -s SIGKILL huly-bridge@muhasebot.service
sudo systemctl is-active 'huly-bridge@*.service'
# muhasebot flips to activating (Restart=on-failure), the rest stay active.
```

## 7. Remove the legacy artifacts

Once every workspace is verified healthy:

```bash
sudo rm -f /etc/systemd/system/huly-bridge.service
sudo rm -f /etc/huly-bridge/bridge.toml
sudo systemctl daemon-reload
```

The Ansible play does this automatically on first run after the
upgrade.

## 8. Roll back

If a workspace is failing and you cannot diagnose in-window, fall back
to the single-instance config you saved in step 1.

```bash
# Stop every per-workspace instance.
sudo systemctl disable --now 'huly-bridge@*.service'

# Restore the legacy unit + config.
sudo install -m 0644 /root/huly-bridge.service.preP6 \
                     /etc/systemd/system/huly-bridge.service
sudo install -m 0640 /root/huly-bridge.toml.preP6 \
                     /etc/huly-bridge/bridge.toml
sudo systemctl daemon-reload
sudo systemctl enable --now huly-bridge.service
```

(Step 1 only saved the config; if you did not also archive the legacy
unit file, grab it from git history:
`git show <pre-P6-sha>:systemd/huly-bridge.service`.)

The template unit file is harmless to leave installed — it is inactive
without an instance enabled.
