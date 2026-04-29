# Ansible deploy

Deploys `huly-bridge` to hosts in the `bridge` inventory group.

## What it does
1. Verifies local release binary + vault config exist.
2. Pushes config (vault-decrypted) to `/etc/huly-bridge/bridge.toml` (mode 0640).
3. Installs systemd unit at `/etc/systemd/system/huly-bridge.service`.
4. Installs binary at `/usr/local/bin/huly-bridge`.
5. `daemon-reload`, restart on change, ensure enabled + started.
6. Verifies `systemctl is-active` and prints recent journal lines.

Idempotent — re-running with no changes triggers no restart.

## First-time setup

1. Pick a vault password and write it to `ansible/.vault-pass` (gitignored), then export it for your shell:
   ```
   echo 'your-strong-password' > ansible/.vault-pass
   chmod 600 ansible/.vault-pass
   export ANSIBLE_VAULT_PASSWORD_FILE="$(pwd)/ansible/.vault-pass"
   ```
   Add the `export` to your shell rc to persist. Alternatively pass `--vault-password-file ansible/.vault-pass` on each ansible command.

2. Encrypt your bridge config into the playbook tree:
   ```
   cp ../config/bridge.toml files/bridge.toml
   ansible-vault encrypt files/bridge.toml
   ```
   The encrypted file is safe to commit.

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
| Edit config | `ansible-vault edit files/bridge.toml` |
| View config | `ansible-vault view files/bridge.toml` |
| Re-key | `ansible-vault rekey files/bridge.toml` |
| Decrypt (don't commit!) | `ansible-vault decrypt files/bridge.toml` |

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

## Per-host config overrides

Drop YAML into `host_vars/<hostname>.yml` to override any var from `group_vars/bridge.yml`
(e.g., a different `bridge_service` name for per-workspace bridges).

## Sudo

The play uses `become: true`. Either:
- Configure passwordless sudo for the deploy user, or
- Run with `--ask-become-pass` (default in the Makefile).
