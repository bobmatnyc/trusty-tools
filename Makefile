# Makefile for the trusty-tools workspace.
#
# Why: provides convenience targets for common maintenance tasks that span
# multiple crates or involve files outside the Cargo build graph.
#
# What: currently exposes `clean-runtime` to delete per-crate runtime
# artefacts that accumulate during development (SQLite analytics DB and
# the trusty-analyze facts redb).
#
# Test: run `make clean-runtime` in a checkout that has these files; assert
# they are deleted and the target exits 0.  Run it again on a clean tree
# and assert it still exits 0 (idempotent).

.PHONY: clean-runtime e2e-docker

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
