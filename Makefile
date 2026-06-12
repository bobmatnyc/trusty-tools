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

.PHONY: clean-runtime

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
