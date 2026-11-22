.PHONY: build test check clippy fmt lint version

# #11632: the Svelte/Tauri UI, the wrapper installer and the REPL/workflow run
# targets are gone with the code they drove. Releases go through `local-ops`
# (see the workspace CLAUDE.md), not through targets here.

VERSION := $(shell cargo metadata --no-deps --format-version 1 | python3 -c "import sys,json; print([p for p in json.load(sys.stdin)['packages'] if p['name']=='trusty-assistants'][0]['version'])")

build:
	cargo build -p trusty-assistants

test:
	cargo test -p trusty-assistants --no-fail-fast

check:
	cargo check -p trusty-assistants

clippy:
	cargo clippy -p trusty-assistants --all-targets -- -D warnings

fmt:
	cargo fmt

lint: clippy fmt

version:
	@echo $(VERSION)
