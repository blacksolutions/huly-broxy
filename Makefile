.PHONY: all linux windows test clippy clean release deploy vault-edit vault-view

DEPLOY_LIMIT ?= bridge
ANSIBLE_DIR ?= ansible
VAULT_FILE ?= files/bridge.toml

all: linux windows

linux:
	cargo build --release --target x86_64-unknown-linux-gnu

windows:
	CROSS_CONTAINER_ENGINE=podman CARGO_TARGET_DIR=target-cross cross build --release --target x86_64-pc-windows-gnu

test:
	cargo test --workspace

clippy:
	cargo clippy --workspace -- -D warnings

clean:
	cargo clean
	rm -rf dist

release: all
	mkdir -p dist/linux-x86_64 dist/windows-x86_64
	cp target/x86_64-unknown-linux-gnu/release/huly-bridge dist/linux-x86_64/
	cp target/x86_64-unknown-linux-gnu/release/huly-mcp dist/linux-x86_64/
	cp target-cross/x86_64-pc-windows-gnu/release/huly-bridge.exe dist/windows-x86_64/
	cp target-cross/x86_64-pc-windows-gnu/release/huly-mcp.exe dist/windows-x86_64/

deploy: linux
	cd $(ANSIBLE_DIR) && ansible-playbook deploy-bridge.yml --limit $(DEPLOY_LIMIT) --ask-become-pass

vault-edit:
	cd $(ANSIBLE_DIR) && ansible-vault edit $(VAULT_FILE)

vault-view:
	cd $(ANSIBLE_DIR) && ansible-vault view $(VAULT_FILE)
