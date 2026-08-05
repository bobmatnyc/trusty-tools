# trusty-memory Makefile
#
# Why: Provides ergonomic single-command workflows for version bumping,
# deploying, and health-checking the trusty-memory daemon on macOS,
# mirroring the trusty-search Makefile pattern.
# What: `patch` bumps version + commits + tags; `deploy` installs the binary
# and restarts both launchd daemons; `smoke` probes the health endpoints.
# Test: `make help` lists all targets; `make smoke` exits 0 when daemon is live.

.PHONY: patch deploy smoke clean check clippy test help

# -- Launchd plist path -------------------------------------------------------
# #4868: this declared `com.bobmatnyc.trusty-memory` canonical — a label no host
# has ever had, alongside a `com.trusty.trusty-memory` legacy that the daemon's
# own constant also does not use. The real unit is `com.trusty.memory`, minted
# by `trusty_common::launchd_labels::MEMORY`. Evicting the old labels belongs to
# `service install`, which does it under launchd with a rollback, so `deploy`
# no longer reimplements the unload/load sequence in shell.
PLIST_CANONICAL := $(HOME)/Library/LaunchAgents/com.trusty.memory.plist

# -- Health-check endpoints ---------------------------------------------------
# Primary: read the bound address from the runtime lock file.
# Fallback: well-known default port used during first-run before lock exists.
SUPPORT_DIR  := $(HOME)/Library/Application Support/trusty-memory
HTTP_ADDR_FILE := $(SUPPORT_DIR)/http_addr
FALLBACK_ADDR  := 127.0.0.1:7070

## Bump the patch version, commit, tag, and push.
## Why: Every patch release produces the canonical `trusty-memory-v<VERSION>`
## tag that triggers the crates.io publish workflow.  Version is read in the
## SAME shell as the bump to avoid Make's parse-time expansion.
patch:
	cargo set-version --bump patch
	@VERSION=$$(cargo metadata --no-deps --format-version 1 \
	  | python3 -c "import sys,json; print(json.load(sys.stdin)['packages'][0]['version'])") && \
	git add Cargo.toml Cargo.lock && \
	git commit -m "chore(release): bump trusty-memory to v$$VERSION" && \
	git tag "trusty-memory-v$$VERSION" && \
	git push origin main && \
	git push origin "trusty-memory-v$$VERSION" && \
	echo ">> pushed trusty-memory-v$$VERSION -- CI will publish to crates.io automatically" && \
	echo ">> run 'make deploy' once CI finishes to install the new binary locally"

## Install the locally-built binary and restart both launchd daemons.
## Why: Replacing the binary while the daemon is running causes macOS to
## SIGKILL the process with "Code Signature Invalid"; unloading first ensures
## a clean handoff.  Both the canonical and legacy plist labels are unloaded
## so stale daemons from either label cannot survive across deploys.
##
## Stop strategy (surgical):
##   1. `trusty-memory service stop` -- SIGTERMs the process that wrote the
##      PID lockfile; never matches by command-line string.
##   2. `pkill -TERM -x trusty-memory` -- matches argv[0] exactly; cannot
##      accidentally match tmux, claude, or other processes.
##
## #4868: the restart is `service install`, not `launchctl load`. Loading a
## plist path directly cannot evict a unit registered under a different label,
## which is how a deploy came to leave the daemon running CLI-detached (the
## `|| trusty-memory service start` fallback) after the load silently failed.
##
## `bootout` runs first so launchd cannot relaunch the daemon into the middle of
## `cargo install` and get its binary swapped underneath it (#87). This unit is
## `KeepAlive::OnSuccess`, so a clean stop would not have been relaunched — but a
## non-zero exit would, and booting the job out removes the question. No
## `--force` is needed here: trusty-memory's `service install` reloads
## unconditionally, so it always activates the binary just built.
deploy:
	-launchctl bootout gui/$$(id -u)/com.trusty.memory 2>/dev/null
	-trusty-memory service stop 2>/dev/null
	-pkill -TERM -x trusty-memory 2>/dev/null
	sleep 2
	CARGO_BUILD_JOBS=2 cargo install --path . --locked
	trusty-memory service install

## Health-check: verify the daemon is reachable and version matches.
## Why: Confirms the deployed binary is the expected version and the HTTP
## server is accepting connections before declaring a deploy successful.
## Probes the address from the runtime lock file first; falls back to the
## well-known default port (7070) if the lock file is absent.
smoke:
	@echo ">> trusty-memory smoke test"
	@ADDR="$(FALLBACK_ADDR)"; \
	if [ -f "$(HTTP_ADDR_FILE)" ]; then \
	  ADDR=$$(cat "$(HTTP_ADDR_FILE)"); \
	  echo ">> using address from http_addr: $$ADDR"; \
	else \
	  echo ">> http_addr file not found; using fallback $$ADDR"; \
	fi; \
	echo ">> probing http://$$ADDR/health"; \
	curl -sf "http://$$ADDR/health" | python3 -m json.tool || \
	  (echo "ERROR: health check failed on $$ADDR" && exit 1)
	@echo ">> also probing fallback http://$(FALLBACK_ADDR)/health (no-op if same)"; \
	curl -sf "http://$(FALLBACK_ADDR)/health" >/dev/null 2>&1 && \
	  echo ">> $(FALLBACK_ADDR) reachable" || \
	  echo "   ($(FALLBACK_ADDR) not reachable -- OK if daemon bound elsewhere)"
	@BIN_VERSION=$$(trusty-memory --version 2>&1 | awk '{print $$2}'); \
	CARGO_VERSION=$$(cargo metadata --no-deps --format-version 1 \
	  | python3 -c "import sys,json; print(json.load(sys.stdin)['packages'][0]['version'])"); \
	echo ">> binary version:  $$BIN_VERSION"; \
	echo ">> Cargo.toml version: $$CARGO_VERSION"; \
	if [ "$$BIN_VERSION" = "$$CARGO_VERSION" ]; then \
	  echo ">> versions match -- smoke PASSED"; \
	else \
	  echo "WARNING: version mismatch (run 'make deploy' to update binary)"; \
	fi

## Remove local build artifacts.
clean:
	cargo clean

## Quick local quality gate -- type-check without full compilation.
check:
	cargo check --workspace

## Run clippy across all targets with warnings as errors.
clippy:
	cargo clippy --workspace --all-targets -- -D warnings

## Run the full test suite.
test:
	cargo test --workspace

help:
	@printf '\ntrustY-memory Makefile targets\n'
	@printf '%-10s %s\n' \
	  patch   "Bump patch version, commit, tag, and push" \
	  deploy  "Build + install binary, restart both launchd daemons" \
	  smoke   "Health-check both ports, verify version matches" \
	  clean   "Remove build artifacts (cargo clean)" \
	  check   "cargo check --workspace" \
	  clippy  "cargo clippy --workspace --all-targets" \
	  test    "cargo test --workspace" \
	  help    "Show this help"
	@printf '\nLaunchd plist managed:\n'
	@printf '  %s\n' "$(PLIST_CANONICAL)"
	@printf '\n'
