.PHONY: build test check clippy release clean version version-full fmt lint ctrl run-task \
        ui ui-dev ui-build ui-check ui-clean ui-web ui-test \
        bump-patch bump-minor bump-major \
        ship ship-minor ship-major \
        install

VERSION := $(shell cargo metadata --no-deps --format-version 1 | python3 -c "import sys,json; print(json.load(sys.stdin)['packages'][0]['version'])")
VERSION_UI := $(shell cargo metadata --no-deps --format-version 1 --manifest-path ui/src-tauri/Cargo.toml 2>/dev/null | python3 -c "import sys,json; print(json.load(sys.stdin)['packages'][0]['version'])" 2>/dev/null || grep '^version' ui/src-tauri/Cargo.toml | head -1 | python3 -c "import sys,re; print(re.search(r'\"(.+)\"', sys.stdin.read()).group(1))")

# ── Core build (harness + UI frontend check) ──────────────────────────────────
# Why: The UI is embedded into the binary via rust-embed at compile time. If
# you run `cargo build` without rebuilding the UI first, the embedded assets
# are stale. After building, `make install` deploys wrapper scripts to
# ~/.local/bin and ~/.cargo/bin so `open-mpm` on PATH always resolves to
# target/release/open-mpm with .env.local auto-loaded.
build: ui-check
	cargo build --release
	@$(MAKE) install

# Build release binary without the UI frontend step.
# Useful for quick iteration when the UI has not changed.
build-release:
	cargo build --release

# Install the freshly built release binary and deploy the wrapper script.
# The wrapper (~/.local/bin/open-mpm and ~/.cargo/bin/open-mpm) loads
# .env.local automatically and supports the --llm-skills convenience flag.
# ~/.cargo/bin takes PATH precedence over ~/.local/bin (rustup puts it first),
# so both locations receive the same wrapper to ensure the correct binary runs.
# Wrapper template: scripts/open-mpm-wrapper.sh
# Install helper:   scripts/install-wrapper.sh
install:
	cargo build --release
	@bash scripts/install-wrapper.sh
	@echo "Restart the API server to load the new binary:"
	@echo "  pkill -f 'open-mpm --api' && open-mpm --api --port 7654 &"

test:
	cargo test

check:
	cargo check

clippy:
	cargo clippy --all-targets --all-features -- -D warnings

fmt:
	cargo fmt

lint: clippy fmt

# ── Release builds both harness binary and Tauri app ─────────────────────────
# Uses the wrapper-script installer (not cargo install --path .) so that
# ~/.cargo/bin/open-mpm remains the .env.local-loading wrapper, not a raw binary.
release: ui-build
	cargo build --release
	@bash scripts/install-wrapper.sh
	@echo "Installed open-mpm wrapper -> ~/.cargo/bin/open-mpm and ~/.local/bin/open-mpm"

version:
	@echo $(VERSION)

version-full:
	@echo "open-mpm:    $(VERSION)"
	@echo "open-mpm-ui: $(VERSION_UI)"

clean: ui-clean
	cargo clean

# ── Version bumping ───────────────────────────────────────────────────────────
# Each bump target runs as a single shell so variable assignments persist.
# Python reads+writes both Cargo.toml files, then cargo generates a fresh lockfile.

bump-patch:
	@python3 scripts/bump-version.py patch
	@cargo generate-lockfile
	@NEW_VER=$$(grep '^version' Cargo.toml | head -1 | python3 -c "import sys,re; print(re.search(r'\"(.+)\"', sys.stdin.read()).group(1))") && \
	CLOSES=$$([ -n "$(ISSUE)" ] && echo " (closes #$(ISSUE))" || echo "") && \
	git add Cargo.toml Cargo.lock && \
	git commit -m "chore: bump to v$$NEW_VER$$CLOSES" && \
	git tag -a "v$$NEW_VER" -m "v$$NEW_VER" && \
	echo "Bumped and tagged v$$NEW_VER$$CLOSES"

bump-minor:
	@python3 scripts/bump-version.py minor
	@cargo generate-lockfile
	@NEW_VER=$$(grep '^version' Cargo.toml | head -1 | python3 -c "import sys,re; print(re.search(r'\"(.+)\"', sys.stdin.read()).group(1))") && \
	CLOSES=$$([ -n "$(ISSUE)" ] && echo " (closes #$(ISSUE))" || echo "") && \
	git add Cargo.toml Cargo.lock && \
	git commit -m "chore: bump to v$$NEW_VER$$CLOSES" && \
	git tag -a "v$$NEW_VER" -m "v$$NEW_VER" && \
	echo "Bumped and tagged v$$NEW_VER$$CLOSES"

bump-major:
	@python3 scripts/bump-version.py major
	@cargo generate-lockfile
	@NEW_VER=$$(grep '^version' Cargo.toml | head -1 | python3 -c "import sys,re; print(re.search(r'\"(.+)\"', sys.stdin.read()).group(1))") && \
	CLOSES=$$([ -n "$(ISSUE)" ] && echo " (closes #$(ISSUE))" || echo "") && \
	git add Cargo.toml Cargo.lock && \
	git commit -m "chore: bump to v$$NEW_VER$$CLOSES" && \
	git tag -a "v$$NEW_VER" -m "v$$NEW_VER" && \
	echo "Bumped and tagged v$$NEW_VER$$CLOSES"

# ── Ship workflow ─────────────────────────────────────────────────────────────
# VERSION is re-read after bump via shell subcommand inside the recipe so it
# reflects the newly written Cargo.toml (Make's $(VERSION) is set at parse-time).

ship: test bump-patch release
	@SHIP_VER=$$(grep '^version' Cargo.toml | head -1 | python3 -c "import sys,re; print(re.search(r'\"(.+)\"', sys.stdin.read()).group(1))") && \
	git add Cargo.toml ui/src-tauri/Cargo.toml && \
	git -c advice.addIgnoredFile=false add Cargo.lock 2>/dev/null || true && \
	git commit -m "chore: bump to v$$SHIP_VER" && \
	git tag -a "v$$SHIP_VER" -m "Release v$$SHIP_VER" && \
	git push && git push --tags && \
	echo "Shipped v$$SHIP_VER"

ship-minor: test bump-minor release
	@SHIP_VER=$$(grep '^version' Cargo.toml | head -1 | python3 -c "import sys,re; print(re.search(r'\"(.+)\"', sys.stdin.read()).group(1))") && \
	git add Cargo.toml ui/src-tauri/Cargo.toml && \
	git -c advice.addIgnoredFile=false add Cargo.lock 2>/dev/null || true && \
	git commit -m "chore: bump to v$$SHIP_VER" && \
	git tag -a "v$$SHIP_VER" -m "Release v$$SHIP_VER" && \
	git push && git push --tags && \
	echo "Shipped v$$SHIP_VER"

ship-major: test bump-major release
	@SHIP_VER=$$(grep '^version' Cargo.toml | head -1 | python3 -c "import sys,re; print(re.search(r'\"(.+)\"', sys.stdin.read()).group(1))") && \
	git add Cargo.toml ui/src-tauri/Cargo.toml && \
	git -c advice.addIgnoredFile=false add Cargo.lock 2>/dev/null || true && \
	git commit -m "chore: bump to v$$SHIP_VER" && \
	git tag -a "v$$SHIP_VER" -m "Release v$$SHIP_VER" && \
	git push && git push --tags && \
	echo "Shipped v$$SHIP_VER"

# ── Tauri UI targets ──────────────────────────────────────────────────────────

# Install UI deps if node_modules missing, then verify frontend compiles
ui-check:
	@if [ ! -d ui/node_modules ]; then cd ui && pnpm install --frozen-lockfile; fi
	cd ui && pnpm build

# Full Tauri desktop app build (produces distributable in ui/src-tauri/target/)
ui-build:
	@if [ ! -d ui/node_modules ]; then cd ui && pnpm install --frozen-lockfile; fi
	cd ui && pnpm tauri build

# Launch Tauri in hot-reload dev mode (also starts open-mpm sidecar)
ui-dev:
	@if [ ! -d ui/node_modules ]; then cd ui && pnpm install --frozen-lockfile; fi
	cd ui && pnpm tauri dev

# Launch the UI as a web app (requires `open-mpm --api` running separately).
# Vite's `/api` proxy forwards requests to VITE_OMPM_PORT (defaults to 7654).
ui-web:
	@echo "Start the API server first: cargo run -- --api --port 7654"
	@if [ ! -d ui/node_modules ]; then cd ui && pnpm install --frozen-lockfile; fi
	cd ui && VITE_OMPM_PORT=7654 pnpm dev

# Run Playwright browser smoke tests against a running open-mpm --api server.
# Requires: server running at OMPM_URL (default http://localhost:7654).
# Usage: make ui-test  OR  OMPM_URL=http://localhost:8080 make ui-test
ui-test:
	@echo "Running Playwright smoke tests against $${OMPM_URL:-http://localhost:7654}..."
	@if [ ! -d ui/node_modules ]; then cd ui && pnpm install --frozen-lockfile; fi
	cd ui && pnpm test

# Convenience alias — just the frontend vite build (faster than full tauri build)
ui:
	cd ui && pnpm build

# Tauri Rust cargo check only (no frontend)
ui-rust-check:
	cd ui/src-tauri && cargo check

ui-clean:
	cd ui && rm -rf dist node_modules src-tauri/target

# ── Run targets ───────────────────────────────────────────────────────────────

# Run with CTRL interactive mode
ctrl:
	cargo run -- --ctrl

# Run with a task file: make run-task TASK_FILE=path/to/task.md
run-task:
	cargo run -- --workflow prescriptive --task-file $(TASK_FILE)
