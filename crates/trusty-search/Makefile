# trusty-search Makefile
#
# Why: `cargo publish` requires the embedded Svelte UI assets to live inside
# the crate-root `ui-dist/`, which `src/service/ui.rs` embeds with `include_dir!`
# to serve `/ui/`. #6155 moved the dashboard's Svelte source out of this crate
# into `crates/trusty-console/ui-search/`, so there is nothing left here to
# build: `ui-dist/` is now a mirror of the bundle the console builds, and
# `sync-ui` is the one command that refreshes it.
# What: `sync-ui` mirrors `crates/trusty-console/ui-search-dist/` into
# `ui-dist/` and re-stamps it; `release-prep` is the documented prerequisite for
# `cargo publish`. Both bundles are stamped against the same source digest, so
# the freshness gate reads them as one.
# Test: `make release-prep` populates `ui-dist/index.html` and the `assets/`
# tree; `bash scripts/check-ui-bundle-freshness.sh trusty-search` then passes.
#
# #6285 deletes this crate's HTTP listener, and `ui-dist/` with it. Until then
# `/ui/` keeps answering exactly what it answers today.

CLOSES      ?=
# Why: paths must resolve to this crate dir regardless of the invoking CWD, so
# `make -C crates/trusty-search sync-ui` from the workspace root produces a
# correct `ui-dist/` (not a nested `ui-dist/dist/`) — see issue #1540.
# What: anchor every UI path to the directory containing THIS Makefile.
MAKEFILE_DIR := $(dir $(abspath $(lastword $(MAKEFILE_LIST))))
REPO_ROOT   := $(MAKEFILE_DIR)../..
UI_SOURCE   := $(REPO_ROOT)/crates/trusty-console/ui-search-dist
UI_EMBED    := $(MAKEFILE_DIR)ui-dist

.PHONY: ui build-ui sync-ui release-prep install patch reinstall deploy check clippy test smoke

## Rebuild the search dashboard bundle, which trusty-console now owns.
build-ui:
	$(MAKE) -C $(REPO_ROOT)/crates/trusty-console search-ui

## Mirror the console's search bundle into ui-dist (no rebuild)
sync-ui:
	@test -f $(UI_SOURCE)/index.html || (echo "ERROR: $(UI_SOURCE) missing — run 'make build-ui' first" && exit 1)
	rm -rf $(UI_EMBED)
	cp -r $(UI_SOURCE) $(UI_EMBED)
	@echo ">> synced $(UI_SOURCE) → $(UI_EMBED)"
	# #3606: the mirror above wipes ui-dist/, so the stamp recording which source
	# this bundle came from is written after it, never before. It also overwrites
	# the console bundle's own stamp, which names the same digest — rewriting it
	# under this crate's row keeps the two rows independently verifiable.
	@bash $(REPO_ROOT)/scripts/stamp-ui-bundle.sh trusty-search

## Build UI + sync (required before cargo publish)
release-prep: build-ui sync-ui
	@echo ">> ui-dist synced. Ready for cargo publish."

## Convenience alias
ui: build-ui

## Install binary from source, stopping any running daemon first (closes #87).
## Why: replacing the binary while the daemon is running causes macOS 26.3+ to
## SIGKILL the process with "Code Signature Invalid"; stopping first ensures a
## clean handoff.  `|| true` makes the stop a no-op when no daemon is running.
install:
	trusty-search stop 2>/dev/null || true
	sleep 1
	cargo install --path . --locked

## Bump the patch version, commit + tag + push, install, and restart the daemon.
## Why: same macOS binary-replacement hazard as `install`; version bump and
## daemon restart are always paired during development patch cycles.
## The git commit/tag/push steps are included here so every patch release
## produces the canonical `v<VERSION>` tag that triggers the crates.io publish
## workflow (.github/workflows/publish.yml matches on `v*` tags).
## Version is read in the SAME shell as the bump (shell var, not Make eval)
## to avoid Make's parse-time expansion of $(eval $(shell ...)).
## `trusty-search start` (no --foreground) self-spawns a detached daemon and
## returns immediately, so this target does not hijack the caller's terminal
## (important when invoked from inside a tmux pane — prior to the self-spawn
## fix, a SIGHUP on pane close would kill the daemon and the tmux session).
patch:
	cargo set-version --bump patch
	@VERSION=$$(cargo metadata --no-deps --format-version 1 \
	  | python3 -c "import sys,json; print(json.load(sys.stdin)['packages'][0]['version'])") && \
	sed -i '' "s/version != [0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*/version != $$VERSION/" .claude/commands/health.md && \
	git add Cargo.toml Cargo.lock .claude/commands/health.md && \
	COMMIT_MSG="chore(release): bump trusty-search to v$$VERSION"; \
	if [ -n "$(CLOSES)" ]; then COMMIT_MSG="$$COMMIT_MSG (closes #$(CLOSES))"; fi; \
	git commit -m "$$COMMIT_MSG" && \
	git tag "v$$VERSION" && \
	git push origin main && \
	git push origin "v$$VERSION" && \
	echo ">> pushed v$$VERSION — CI will publish to crates.io automatically" && \
	echo ">> run 'make deploy' once CI finishes to install the new binary locally"

## Install the locally-built binary with reduced parallelism to avoid OOM.
## Stops running trusty-search instances (launchd + manual) before
## compiling so the compiler doesn't compete with the daemon for RAM.
##
## Stop strategy (surgical — cannot affect tmux or claude-mpm):
##   1. `trusty-search stop` — reads the PID lockfile and SIGTERMs exactly
##      the process that wrote it; never matches by command-line string.
##   2. `pkill -TERM -x trusty-search` — matches the process NAME exactly
##      (not a substring of the full command line), so it only hits processes
##      whose argv[0] is literally "trusty-search". The `-x` flag is POSIX
##      "exact name match"; it cannot match tmux, claude, or any script that
##      merely contains the string "trusty-search" somewhere in its arguments.
##
## Canonical launchd label: com.trusty.search — the unit launchd actually has
## loaded, and the value `trusty_common::launchd_labels::SEARCH` mints.
##
## #4868: this target used to declare `com.bobmatnyc.trusty-search` canonical,
## a THIRD family of label beside the daemon's own constant and the installer's
## mirror table. Nothing by that name has ever existed on a host, so every
## deploy ran `launchctl unload` against a missing file, killed the daemon with
## `stop` + `pkill`, then failed to `load` it back and fell through to
## `trusty-search start` — a CLI-detached daemon, unsupervised, after a gap that
## spanned the whole `cargo install`. That gap is the release outage.
##
## The eviction of the legacy labels now belongs to `service install`, which
## does it under launchd with a rollback. This target defers to it instead of
## reimplementing the sequence in shell.
##
## Two things this sequence must get right, both flagged in #4868 review:
##
##  1. `bootout` FIRST, not `stop` + `pkill`. The installed unit is
##     `KeepAlive::Always` (#4113), so a SIGTERM'd daemon is relaunched by
##     launchd within `ThrottleInterval` — `cargo install` would then overwrite
##     the binary of a running process, which is #87 (macOS SIGKILLs it with
##     "Code Signature Invalid"). Booting the job out deregisters it, so nothing
##     brings it back until we ask.
##  2. `--force` on the way back in. The rendered plist is byte-identical
##     across a deploy — only the BINARY changed — so without it
##     `install_and_activate` reports `AlreadyCurrent` and launchd keeps running
##     the old image. A deploy that does not activate what it built is not a
##     deploy.
##     The LEGACY label is booted out too. On a mid-migration host the loaded
##     unit is still `com.trusty.trusty-search` with `KeepAlive::Always`, so
##     booting out only the canonical label leaves that daemon running straight
##     into the binary swap — the very hazard (1) exists to prevent. The old
##     recipe covered this with `unload $(PLIST_LEGACY)`; dropping it without
##     replacement would have been a regression.
deploy:
	-launchctl bootout gui/$$(id -u)/com.trusty.search 2>/dev/null
	-launchctl bootout gui/$$(id -u)/com.trusty.trusty-search 2>/dev/null
	-trusty-search stop 2>/dev/null
	-pkill -TERM -x trusty-search 2>/dev/null
	sleep 2
	CARGO_BUILD_JOBS=2 cargo install --path . --locked
	trusty-search service install --force

## Stop daemon, install new binary from source, restart (closes #87)
## Why: replacing the binary while the daemon is running causes macOS to
## SIGKILL the process; stopping first ensures a clean handoff.
## `trusty-search start` self-spawns a detached daemon and returns
## immediately, so this target does not block the caller.
reinstall:
	trusty-search stop 2>/dev/null || true
	sleep 2
	CARGO_BUILD_JOBS=2 cargo install --path . --locked
	trusty-search start

## Quick local quality gate
check:
	cargo check --workspace

clippy:
	cargo clippy --workspace --all-targets -- -D warnings

test:
	cargo test --workspace
