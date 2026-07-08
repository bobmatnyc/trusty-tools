# Makefile for the trusty-tools workspace.
#
# Why: provides convenience targets for common maintenance tasks that span
# multiple crates or involve files outside the Cargo build graph.
#
# What: currently exposes `clean-runtime` to delete per-crate runtime
# artefacts that accumulate during development (SQLite analytics DB and
# the trusty-analyze facts redb), plus `publish-check` to gate publishing.
#
# Test: run `make clean-runtime` in a checkout that has these files; assert
# they are deleted and the target exits 0.  Run it again on a clean tree
# and assert it still exits 0 (idempotent).

.PHONY: check clean-runtime e2e-docker install-search-signed publish-check

# Remove per-crate runtime artefacts that accumulate during development.
#
# Why: `tga.db` (trusty-git-analytics analytics SQLite database) and
# `trusty-analyze.facts.redb` (trusty-analyze facts store) grow
# unboundedly across development sessions.  Deleting them forces a clean
# rebuild of both on the next daemon start, which is useful when schema
# migrations misbehave or disk pressure is a concern.
#
# What: deletes `tga.db` and `trusty-analyze.facts.redb` from the
# workspace root if they exist; exits 0 whether or not the files were
# present (idempotent).
clean-runtime:
	@echo "Removing runtime artefacts..."
	@rm -f tga.db trusty-analyze.facts.redb
	@echo "Done."

# Run the documented workspace quality gate referenced throughout CLAUDE.md.
#
# Why: CLAUDE.md instructs contributors to run the test/lint/format gate
# before committing, but until now there was no single target to invoke —
# every contributor had to remember and chain the three commands by hand.
#
# What: runs `cargo test --workspace`, then `cargo clippy --workspace
# --all-targets -- -D warnings`, then `cargo fmt --check`, in that order,
# failing fast on the first failing step (default Make behaviour).
#
# Test: `make check` on a clean, formatted, warning-free workspace exits 0.
# Introduce a clippy warning or an unformatted file and confirm `make check`
# exits non-zero at the corresponding step.
check:
	cargo test --workspace
	cargo clippy --workspace --all-targets -- -D warnings
	cargo fmt --check

# Run the Docker-based install-from-crates.io E2E smoke test for all four
# trusty-* tools.
#
# Why: verifies that published crates install cleanly on a clean Linux base
# image and pass minimal end-to-end scenarios, catching regressions in the
# published package before users hit them.
# What: builds the trusty-e2e Docker image and runs the four-scenario
# smoke test (trusty-search, trusty-memory, trusty-mpm, trusty-analyze).
# Test: `make e2e-docker` should exit 0 when all scenarios pass. Run from
# the workspace root; Docker must be installed and running.
#
# Override tool versions (empty = latest published):
#   make e2e-docker TRUSTY_SEARCH_VERSION=0.24.10
e2e-docker:
	bash scripts/e2e-docker.sh \
		$(if $(TRUSTY_SEARCH_VERSION),--search-version $(TRUSTY_SEARCH_VERSION)) \
		$(if $(TRUSTY_MEMORY_VERSION),--memory-version $(TRUSTY_MEMORY_VERSION)) \
		$(if $(TRUSTY_MPM_VERSION),--mpm-version $(TRUSTY_MPM_VERSION)) \
		$(if $(TRUSTY_ANALYZE_VERSION),--analyze-version $(TRUSTY_ANALYZE_VERSION))

# Install trusty-search with Developer ID signing for persistent macOS FDA grant.
#
# Why: fixes #873 — `cargo install` produces a new cdhash (ad-hoc signing) that
# causes macOS TCC to revoke the Full Disk Access grant after every reinstall.
# Signing with a Developer ID Application cert + fixed --identifier per binary
# makes the designated requirement (DR) stable, so FDA persists across reinstalls.
#
# What: delegates to scripts/install-trusty-search-signed.sh which runs
# `cargo install`, auto-detects the Developer ID identity (or prints setup
# instructions if none found), codesigns both trusty-search and trusty-embedderd,
# verifies the signatures, and prints next-steps for the one-time FDA grant and
# daemon restart.
#
# Test: run `make install-search-signed`; assert both binaries are signed with
# a Developer ID (not ad-hoc) and `codesign --verify --strict` passes for each.
# Run without a Developer ID cert installed to verify it exits non-zero and
# prints actionable cert setup guidance.
#
# Overrides (passed as env vars, not make vars):
#   TRUSTY_SIGN_IDENTITY=...       override the auto-detected signing identity
#   TRUSTY_INSTALL_PATH=...        install from an explicit repo checkout path
#   TRUSTY_INSTALL_VERSION=X.Y.Z   install a specific crates.io published version
#   TRUSTY_CODESIGN_DRY_RUN=1     skip cargo install + codesign, test guidance only
install-search-signed:
	bash scripts/install-trusty-search-signed.sh

# Publish-only-from-merged-main preflight guard (issue #2227).
#
# Why: issue #2209 published from an unmerged branch, making a P0-missing
# build the crates.io "latest" for every concurrent session. This gate makes
# "publish only from merged main, with the release tag present" a mechanical
# check instead of a documented convention someone can forget under pressure.
#
# What: delegates to scripts/check-publish-ready.sh CRATE, which asserts HEAD
# is an ancestor of the TRUE origin/main tip and that the release tag
# <prefix>-v<version> exists there too. Escape hatch (deliberate, rare use
# only): ALLOW_UNMERGED_PUBLISH=1.
#
# Test: `make publish-check CRATE=trusty-mpm` from a checkout at merged main
# exits 0; from an unmerged branch it exits non-zero with an actionable
# message. `ALLOW_UNMERGED_PUBLISH=1 make publish-check CRATE=<crate>`
# downgrades failures to a warning and exits 0.
#
# Usage:
#   make publish-check CRATE=trusty-mpm
publish-check:
	@if [ -z "$(CRATE)" ]; then \
		echo "Usage: make publish-check CRATE=<crate-name-or-dir>" >&2; \
		exit 2; \
	fi
	bash scripts/check-publish-ready.sh $(CRATE)
