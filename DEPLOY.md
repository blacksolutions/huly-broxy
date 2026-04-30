# Deployment Guide

Multi-workspace deployment: 1x NATS, Nx huly-bridge, 1x huly-mcp.

> **Platforms.** Sections 1–9 describe the **Linux** production deployment, which the development team builds and tests. Section 10 documents the **Windows** deployment; Windows binaries are cross-compiled but have **not** been validated by the dev team and are pending QA sign-off.

```
                    Huly.io Platform
                   /       |        \
          [bridge:ws1] [bridge:ws2] [bridge:wsN]
            :9090        :9091        :9092
                   \       |        /
                    NATS (podman, localhost:4222)
                          |
                     [huly-mcp]  (spawned by Claude via stdio)
                          |
                     Claude Code
```

---

## 1. Prerequisites

### System packages

**Fedora / RHEL:**

```bash
sudo dnf install gcc openssl-devel pkg-config
```

**Debian / Ubuntu:**

```bash
sudo apt install build-essential libssl-dev pkg-config
```

### Rust toolchain

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup update stable
```

Requires Rust edition 2024 (stable 1.85+).

### Podman (for NATS)

NATS runs as a rootless Podman container managed by systemd (Quadlet).

```bash
# Fedora / RHEL
sudo dnf install podman

# Debian / Ubuntu
sudo apt install podman
```

Requires Podman 4.4+ for Quadlet support. Debian stable ships Podman 4.3; use the Kubic project repo or `bookworm-backports` to obtain 4.4+.

---

## 2. Build

```bash
cd /path/to/huly-kube

# Build all crates in release mode
cargo build --release

# Run tests to verify
cargo test --workspace

# Lint check
cargo clippy --workspace
```

Output binaries:

```
target/release/huly-bridge
target/release/huly-mcp
```

### Install binaries

```bash
sudo install -m 0755 target/release/huly-bridge /usr/local/bin/
sudo install -m 0755 target/release/huly-mcp   /usr/local/bin/
```

---

## 3. NATS Setup (Rootless Podman Quadlet)

NATS runs as a rootless Podman container under a dedicated `nats` system user. JetStream is enabled with a persistent volume at `/var/lib/nats/data`. The container publishes `127.0.0.1:4222` (client) and `127.0.0.1:8222` (monitoring), so bridge/MCP keep using `nats://127.0.0.1:4222` with no config change.

### 3.1 One-time host setup

```bash
# Create dedicated user (login shell required so machinectl/sudo -iu work)
sudo useradd --system --create-home --shell /bin/bash nats

# Enable user services to start at boot without an active session
sudo loginctl enable-linger nats

# Start the user manager now so `systemctl --user` works in §3.3
sudo systemctl start "user@$(id -u nats).service"

# Persistent JetStream storage
sudo mkdir -p /var/lib/nats/data
sudo chown -R nats:nats /var/lib/nats

# Verify subuid/subgid were allocated (required for rootless)
grep nats /etc/subuid /etc/subgid
# If empty:
#   sudo usermod --add-subuids 100000-165535 --add-subgids 100000-165535 nats
```

### 3.2 Install the Quadlet unit

Run from the repo root:

```bash
sudo install -d -o nats -g nats -m 0755 /var/lib/nats/.config/containers/systemd
sudo install -o nats -g nats -m 0644 \
  "$(pwd)/systemd/nats.container" \
  /var/lib/nats/.config/containers/systemd/nats.container
```

Quadlet picks the unit up from the `nats` user's `~/.config/containers/systemd/` and generates a `nats.service` in that user's systemd manager.

### 3.3 Start NATS

```bash
# Run systemctl as the nats user
sudo machinectl shell nats@ /bin/bash -lc \
  'systemctl --user daemon-reload && systemctl --user start nats.service'
```

If `machinectl` is not available:

```bash
sudo -iu nats env XDG_RUNTIME_DIR=/run/user/$(id -u nats) \
  systemctl --user daemon-reload
sudo -iu nats env XDG_RUNTIME_DIR=/run/user/$(id -u nats) \
  systemctl --user start nats.service
```

### 3.4 Verify

```bash
# Container running
sudo machinectl shell nats@ /bin/bash -lc 'podman ps'

# Service status
sudo machinectl shell nats@ /bin/bash -lc 'systemctl --user status nats.service'

# Client port listening
ss -ltnp | grep 4222

# Monitoring endpoint
curl -s http://127.0.0.1:8222/varz | head
```

NATS listens on `nats://127.0.0.1:4222`.

---

## 4. Bridge Deployment (per workspace)

Each workspace runs as its own systemd template-instance:
`huly-bridge@<workspace>.service`. One template unit, N instances, each
with its own config file, dynamic user, port, and journal stream. A
panic in one workspace's bridge cannot affect another's.

> **Note:** the template unit does not declare `After=nats.service`. The bridge starts independently and relies on its internal connection retry loop, so ordering against the rootless NATS user unit is unnecessary.

> **Upgrading from a single-instance bridge?** Follow
> [`docs/operations/migration-per-workspace.md`](docs/operations/migration-per-workspace.md)
> for the cutover and rollback path.

### 4.1 Create config

```bash
sudo install -d -m 0755 /etc/huly-bridge
sudo install -d -m 0755 /etc/huly-bridge/workspaces.d
```

Create `/etc/huly-bridge/workspaces.d/<workspace>.toml` per workspace. Filename MUST match `[huly] workspace` — the template unit substitutes `%i` from the instance name. Example for workspace `acme`:

```toml
[huly]
url = "https://huly.example.com"
workspace = "acme"
use_binary_protocol = true
use_compression = true

[huly.auth]
method = "token"
token = "acme-api-token"

[nats]
url = "nats://127.0.0.1:4222"
subject_prefix = "huly"

[admin]
host = "127.0.0.1"
port = 9090              # <-- unique per workspace
api_token = "your-shared-secret"  # required — MCP must use the same value

[log]
level = "info"
json = false
```

Second workspace `beta` — same file, different workspace/token/port:

```toml
[huly]
url = "https://huly.example.com"
workspace = "beta"

[huly.auth]
method = "token"
token = "beta-api-token"

[nats]
url = "nats://127.0.0.1:4222"
subject_prefix = "huly"

[admin]
host = "127.0.0.1"
port = 9091              # <-- different port
api_token = "your-shared-secret"  # same token as MCP config

[log]
level = "info"
```

**Important:**
- Each bridge must have a unique `[admin] port`.
- `api_token` is required — without it, `/api/v1/*` endpoints return 403. All bridges and the MCP server must share the same token value.

Lock down config files — they contain auth tokens/passwords:

```bash
sudo chmod 0640 /etc/huly-bridge/workspaces.d/*.toml
sudo chown root:root /etc/huly-bridge/workspaces.d/*.toml
```

The template unit uses `DynamicUser=yes` with `ReadOnlyPaths=/etc/huly-bridge`, so the dynamic user can read but not modify these files.

### 4.2 Install the template unit

The template is installed once; instances are spawned per workspace.

```bash
sudo install -m 0644 systemd/huly-bridge@.service /etc/systemd/system/
sudo systemctl daemon-reload
```

### 4.3 Enable and start one instance per workspace

```bash
sudo systemctl enable --now huly-bridge@acme.service
sudo systemctl enable --now huly-bridge@beta.service
```

The `%i` in the unit's `ExecStart` resolves to `acme` / `beta` and
points at `/etc/huly-bridge/workspaces.d/<ws>.toml`.

### 4.4 Check status

```bash
# Per-instance status
sudo systemctl status huly-bridge@acme.service
sudo systemctl status huly-bridge@beta.service

# All instances at once
sudo systemctl list-units 'huly-bridge@*.service'

# Logs (per workspace)
journalctl -u huly-bridge@acme.service -f
journalctl -u huly-bridge@beta.service -f

# Health endpoints
curl http://127.0.0.1:9090/readyz   # acme
curl http://127.0.0.1:9091/readyz   # beta
```

---

## 5. MCP Setup

The MCP server is not a daemon. Claude Code spawns it on demand via stdio.

### 5.1 Create config

```bash
sudo mkdir -p /etc/huly-mcp
sudo chmod 0755 /etc/huly-mcp
```

Create `/etc/huly-mcp/mcp.toml`:

```toml
[nats]
url = "nats://127.0.0.1:4222"

[mcp]
# Required (P4 / D8): logged by the bridge JWT broker for audit + rate-limit
# attribution. Failure to set fails startup with a clear error.
agent_id = "claude-code-<host>-<n>"
transport = "rest"  # the only implemented variant; see P1 spike

[log]
level = "info"
```

```bash
sudo chmod 0600 /etc/huly-mcp/mcp.toml
sudo chown root:root /etc/huly-mcp/mcp.toml
```

Lock `mcp.toml` down with `0600` — `agent_id` is sensitive (it shows up in
audit logs the bridge keeps for the JWT broker).

### 5.2 Claude Code integration

Add to your `.mcp.json` (project root or `~/.claude/.mcp.json`):

```json
{
  "mcpServers": {
    "huly": {
      "command": "/usr/local/bin/huly-mcp",
      "args": ["--config", "/etc/huly-mcp/mcp.toml"]
    }
  }
}
```

The MCP server will:
- Connect to NATS.
- Mint workspace JWTs on demand via `huly.bridge.mint` (P4 / D10).
- Expose the basic tools: `huly_list_workspaces`, `huly_status`,
  `huly_find`, `huly_get`, `huly_create`, `huly_update`, `huly_delete`.
  (Tracker / markup / sync tools land in P5 against the new direct path.)

---

## 6. Verification

### All bridges discovered

Use Claude Code and call the `huly_list_workspaces` MCP tool. It should list all running bridges with their workspace names and status.

### Health check script

```bash
#!/bin/bash
# check-bridges.sh — verify all bridges are ready

PORTS=(9090 9091 9092)  # add your ports
for port in "${PORTS[@]}"; do
  status=$(curl -s -o /dev/null -w "%{http_code}" "http://127.0.0.1:${port}/readyz")
  if [ "$status" = "200" ]; then
    echo "Bridge :${port} — OK"
  else
    echo "Bridge :${port} — DOWN (HTTP ${status})"
  fi
done
```

### Prometheus metrics

Each bridge exposes metrics at `http://127.0.0.1:<port>/metrics`:

```
huly_bridge_events_forwarded_total
huly_bridge_events_failed_total
huly_bridge_ws_connected
huly_bridge_nats_connected
```

---

## 7. Adding a New Workspace

1. Drop a config at `/etc/huly-bridge/workspaces.d/<workspace>.toml`
   (filename must match `[huly] workspace`; assign a unique
   `[admin] port`).
2. `sudo systemctl enable --now huly-bridge@<workspace>.service`.

No unit file edits, no `daemon-reload` needed — the template handles
new instances.

The MCP server discovers new bridges automatically within 10 seconds (announcement interval).

---

## 8. Removing a Workspace

```bash
sudo systemctl disable --now huly-bridge@<workspace>.service
sudo rm /etc/huly-bridge/workspaces.d/<workspace>.toml
```

The MCP server drops stale bridges after `stale_timeout_secs` (default 30s).

---

## 9. Security Considerations

### CORS (S5)

The admin API intentionally does **not** configure CORS headers. It is designed for server-to-server communication (MCP server, Prometheus, health probes) — not browser access. If browser clients need access in the future, add `tower-http`'s `CorsLayer`.

### Token in WebSocket URL (S9)

The Huly WebSocket protocol embeds the session token in the URL path (`wss://endpoint/{token}?sessionId=...`), matching the official TypeScript client. This is safe over WSS but the token may appear in reverse proxy access logs.

**Mitigation:** Configure your reverse proxy to scrub or mask URL paths in access logs. For Traefik, set `accessLog.fields.headers.names.Authorization = "redact"` and consider a middleware that strips the path from logged URLs. For nginx, use a custom `log_format` that omits `$request_uri`.

---

## 10. Windows Deployment (Awaiting QA Validation)

> **Status:** The development team does not have a Windows test environment. The binaries build cleanly via `cross` + MinGW, but runtime behavior on Windows has **not** been verified. This section exists so QA can install and validate the Windows build. Do **not** treat this as a supported production path until QA signs off.

### 10.1 What's different from Linux

| Concern | Linux | Windows |
|---------|-------|---------|
| Service manager | `systemd` (`Type=notify`, `sd_notify`, watchdog) | Windows Service Manager (no `sd_notify`) |
| Readiness integration | systemd notify socket | **Not wired up** — bridge will run, but service manager has no readiness signal |
| Watchdog | `WATCHDOG=1` pings | **Not wired up** — no automatic restart on WS/NATS loss beyond process crash |
| Config path (suggested) | `/etc/huly-bridge/workspaces.d/<ws>.toml` | `C:\ProgramData\huly-bridge\<ws>.toml` |
| Binary install (suggested) | `/usr/local/bin/` | `C:\Program Files\huly-bridge\` |
| NATS | Rootless Podman Quadlet | Run NATS natively, in Docker Desktop, or point at a Linux NATS host |

The admin API, metrics, and MCP stdio server work identically on both platforms — only the service lifecycle integration differs.

### 10.2 Build (from the Linux dev host)

```bash
# Requires: cross (cargo install cross), podman
make windows

# Output
target-cross/x86_64-pc-windows-gnu/release/huly-bridge.exe
target-cross/x86_64-pc-windows-gnu/release/huly-mcp.exe

# Or produce a distributable tree for both OSes
make release
# dist/windows-x86_64/huly-bridge.exe
# dist/windows-x86_64/huly-mcp.exe
```

Transfer `dist/windows-x86_64/` to the Windows QA machine.

### 10.3 Runtime prerequisites (Windows)

The binaries are linked against MinGW's GNU runtime and statically bundle OpenSSL replacement (`rustls`), so **no Visual C++ redistributable is required**. QA should confirm this on a clean Windows install.

- Windows 10 / Server 2019 or newer, x86_64.
- NATS reachable from the host — either:
  - **Local:** run `nats-server.exe` (download from https://nats.io/download/) as a service, or
  - **Remote:** point `[nats] url` at an existing NATS endpoint.
- For MCP integration, Claude Code Desktop or any MCP-capable client installed on the same host.

### 10.4 Install binaries and config

```powershell
# Run as Administrator

# Binaries
New-Item -ItemType Directory -Force "C:\Program Files\huly-bridge" | Out-Null
Copy-Item .\huly-bridge.exe "C:\Program Files\huly-bridge\"
Copy-Item .\huly-mcp.exe    "C:\Program Files\huly-bridge\"

# Config directory (restrict to Administrators + SYSTEM)
New-Item -ItemType Directory -Force "C:\ProgramData\huly-bridge" | Out-Null
icacls "C:\ProgramData\huly-bridge" /inheritance:r /grant:r "Administrators:(OI)(CI)F" "SYSTEM:(OI)(CI)F"
```

Create `C:\ProgramData\huly-bridge\acme.toml` using the same schema documented in §4.1 (paths in the TOML are not OS-specific — only the file location changes).

### 10.5 Register as a Windows Service

The bridge binary does not know how to talk to the Windows Service Control Manager, so wrap it with a service shim. [NSSM](https://nssm.cc/) is the straightforward option:

```powershell
# Install NSSM (choco install nssm) then:
nssm install HulyBridgeAcme "C:\Program Files\huly-bridge\huly-bridge.exe"
nssm set    HulyBridgeAcme AppParameters "--config C:\ProgramData\huly-bridge\acme.toml"
nssm set    HulyBridgeAcme AppStdout    "C:\ProgramData\huly-bridge\acme.out.log"
nssm set    HulyBridgeAcme AppStderr    "C:\ProgramData\huly-bridge\acme.err.log"
nssm set    HulyBridgeAcme AppRotateFiles 1
nssm set    HulyBridgeAcme Start SERVICE_AUTO_START
nssm start  HulyBridgeAcme
```

Repeat per workspace with a unique service name and config path. `sc.exe create` works too, but NSSM handles log rotation and restart-on-failure without extra scripting.

**Known gap:** without the systemd notify/watchdog integration, Windows has no way to detect "bridge is up but degraded." QA should rely on the `/readyz` endpoint (§4.4) and the Prometheus metrics (§6) for health monitoring instead of the service state.

### 10.6 MCP on Windows

Edit Claude Code's `.mcp.json` (typically `%USERPROFILE%\.claude\.mcp.json`):

```json
{
  "mcpServers": {
    "huly": {
      "command": "C:\\Program Files\\huly-bridge\\huly-mcp.exe",
      "args": ["--config", "C:\\ProgramData\\huly-bridge\\mcp.toml"]
    }
  }
}
```

The MCP server is stdio-only; no service registration needed.

### 10.7 QA Validation Checklist

Please verify each item and report pass/fail. Items marked **(blocker)** must pass before we ship a Windows release.

**Build & install**
- [ ] `huly-bridge.exe --help` runs on a clean Windows machine without missing-DLL errors **(blocker)**
- [ ] `huly-mcp.exe --help` runs on a clean Windows machine without missing-DLL errors **(blocker)**
- [ ] Config file parses (`huly-bridge.exe --config <path>`); bad TOML produces a clear error

**Bridge runtime**
- [ ] Bridge connects to Huly over WSS (check journal/log for `hello` handshake success) **(blocker)**
- [ ] Bridge connects to NATS **(blocker)**
- [ ] `GET http://127.0.0.1:9090/healthz` returns `200`
- [ ] `GET http://127.0.0.1:9090/readyz` returns `200` when both WS and NATS are up
- [ ] `GET http://127.0.0.1:9090/metrics` returns Prometheus text
- [ ] Event forwarding: trigger a change in Huly, confirm a message lands on `huly.events.*` in NATS **(blocker)**
- [ ] Kill the NATS process: `readyz` flips to `503`, `huly_bridge_nats_connected` goes to `0`, bridge reconnects when NATS returns **(blocker)**
- [ ] Kill WS (simulate by blocking the Huly URL in the firewall): bridge reconnects with backoff
- [ ] `Ctrl+C` / `nssm stop HulyBridgeAcme` shuts the bridge down within 10s

**Service integration**
- [ ] Service starts automatically after reboot (`Start SERVICE_AUTO_START`) **(blocker)**
- [ ] Service auto-restarts after a forced process kill (`taskkill /F`)
- [ ] `acme.out.log` / `acme.err.log` are written and rotated

**MCP**
- [ ] Claude Code spawns `huly-mcp.exe` successfully
- [ ] `huly_list_workspaces` returns the registered bridge(s) **(blocker)**
- [ ] `huly_find`, `huly_get`, `huly_create`, `huly_update`, `huly_delete` each succeed against a test workspace **(blocker)**

**Security / hardening**
- [ ] `C:\ProgramData\huly-bridge\*.toml` is not readable by non-admin users
- [ ] Admin API bound to `127.0.0.1` is not reachable from another host on the LAN

Report results to the dev team with the Windows build version (`huly-bridge.exe --version`) and Windows edition/build number.
