# Ansible deploy

Deploys `huly-bridge` to hosts in the `bridge` inventory group as one
template-instance per workspace (`huly-bridge@<ws>.service`).

## What it does

1. Verifies local release binary + per-workspace vault configs exist.
2. Installs the systemd template unit at
   `/etc/systemd/system/huly-bridge@.service`.
3. Renders each workspace's vault-decrypted config to
   `/etc/huly-bridge/workspaces.d/<ws>.toml` (mode 0640).
4. Installs the binary at `/usr/local/bin/huly-bridge`.
5. `daemon-reload` + `enable --now huly-bridge@<ws>` for each workspace
   in `bridge_workspaces`.
6. Disables and removes the legacy single-instance unit / config if
   present (one-shot upgrade cleanup).
7. Verifies `systemctl is-active huly-bridge@<ws>` and prints recent
   journal lines per instance.

Idempotent — re-running with no changes triggers no restart. A changed
config restarts only that workspace's instance.

## Vault layout

One vault file per workspace:

```
ansible/files/workspaces/
  muhasebot.toml          # vault-encrypted; safe to commit
  beta.toml               # vault-encrypted
  ...
```

Per-workspace files (rather than a single shared vault) so credentials
rotate independently — re-encrypting one workspace does not touch the
others, and a leak of one vault key compromises only one workspace's
secrets.

## First-time setup

1. Pick a vault password and write it to `ansible/.vault-pass`
   (gitignored), then export it:
   ```
   echo 'your-strong-password' > ansible/.vault-pass
   chmod 600 ansible/.vault-pass
   export ANSIBLE_VAULT_PASSWORD_FILE="$(pwd)/ansible/.vault-pass"
   ```

2. Create + encrypt a config for each workspace listed in
   `group_vars/bridge.yml :: bridge_workspaces`:
   ```
   cp ../config/workspaces.d/muhasebot.toml.example \
      files/workspaces/muhasebot.toml
   $EDITOR files/workspaces/muhasebot.toml
   ansible-vault encrypt files/workspaces/muhasebot.toml
   ```
   Repeat for each workspace. Encrypted files are safe to commit.

3. Confirm SSH reaches the host:
   ```
   ansible -m ping bridge
   ```

## Deploy

```
make deploy            # builds linux release + runs playbook
```

Or directly:
```
cd ansible && ansible-playbook deploy-bridge.yml --limit bridge --ask-become-pass
```

## Vault commands

| Action | Command |
| --- | --- |
| Edit one workspace | `ansible-vault edit files/workspaces/<ws>.toml` |
| View one workspace | `ansible-vault view files/workspaces/<ws>.toml` |
| Re-key one workspace | `ansible-vault rekey files/workspaces/<ws>.toml` |
| Re-key all | `for f in files/workspaces/*.toml; do ansible-vault rekey "$f"; done` |

## Adding a workspace

1. Drop `<ws>.toml` (vault-encrypted) into `files/workspaces/`.
2. Append `<ws>` to `bridge_workspaces` in `group_vars/bridge.yml` (or a
   per-host override in `host_vars/<hostname>.yml`).
3. Re-run `make deploy`. The playbook renders the config and enables
   `huly-bridge@<ws>.service` without disturbing other instances.

## Adding a host

Edit `inventory.yml`:
```yaml
all:
  children:
    bridge:
      hosts:
        riven:
          ansible_host: riven
        new-host:
          ansible_host: new.example.com
          ansible_user: deploy
```

Per-host workspace overrides go in `host_vars/<hostname>.yml`:
```yaml
bridge_workspaces:
  - muhasebot
  - tenant-a
```

## Sudo

The play uses `become: true`. Either:
- Configure passwordless sudo for the deploy user, or
- Run with `--ask-become-pass` (default in the Makefile).
