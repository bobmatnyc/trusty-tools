# trusty-console Makefile
#
# Why: this crate packages FOUR committed UI bundles, and build.rs keeps all
# four current. `ui/dist/` is the console's own management app;
# `ui-search-dist/` is the search dashboard it serves at /tools/search/,
# `ui-memory-dist/` the memory dashboard at /tools/memory/ and
# `ui-analyze-dist/` the analyze dashboard at /tools/analyze/ (#6155). Until
# #6155 those three were built from another crate's source, which build.rs
# deliberately did not reach into. All three sources now live in this crate, so
# build.rs builds them like any other, and these targets exist for the two
# callers that ask for a rebuild by name: an operator preparing a release, and
# check-ui-bundle-freshness.sh's remedy line.
# What: `search-ui` rebuilds `ui-search-dist/` from `ui-search/`, `memory-ui`
# rebuilds `ui-memory-dist/` from `ui-memory/` and `analyze-ui` rebuilds
# `ui-analyze-dist/` from `ui-analyze/`; each Vite config writes to its own
# directory and re-stamps it, so no mirror step follows. `release-prep` is the
# name check-ui-bundle-freshness.sh names in its remedy.
# Test: after `make -C crates/trusty-console release-prep`,
# `bash scripts/check-ui-bundle-freshness.sh trusty-console` passes all four
# rows.

MAKEFILE_DIR := $(dir $(abspath $(lastword $(MAKEFILE_LIST))))
REPO_ROOT    := $(MAKEFILE_DIR)../..
SEARCH_UI    := $(MAKEFILE_DIR)ui-search
MEMORY_UI    := $(MAKEFILE_DIR)ui-memory
ANALYZE_UI   := $(MAKEFILE_DIR)ui-analyze

.PHONY: search-ui memory-ui analyze-ui release-prep

## Rebuild the embedded search dashboard and re-stamp it.
##
## `pnpm build` is enough: ui-search/vite.config.js sets `outDir` to the
## crate-root ui-search-dist/ and runs the stamp plugin on closeBundle.
search-ui:
	@if command -v pnpm > /dev/null 2>&1; then \
	  echo ">> pnpm install && pnpm build (in $(SEARCH_UI))"; \
	  cd $(SEARCH_UI) && pnpm install --frozen-lockfile && pnpm build; \
	else \
	  echo ">> npm ci && npm run build (in $(SEARCH_UI))"; \
	  cd $(SEARCH_UI) && npm ci && npm run build; \
	fi

## Rebuild the embedded memory dashboard and re-stamp it.
##
## `pnpm build` is enough: ui-memory/vite.config.js sets `outDir` to the
## crate-root ui-memory-dist/ and runs the stamp plugin on closeBundle.
memory-ui:
	@if command -v pnpm > /dev/null 2>&1; then \
	  echo ">> pnpm install && pnpm build (in $(MEMORY_UI))"; \
	  cd $(MEMORY_UI) && pnpm install --frozen-lockfile && pnpm build; \
	else \
	  echo ">> npm ci && npm run build (in $(MEMORY_UI))"; \
	  cd $(MEMORY_UI) && npm ci && npm run build; \
	fi

## Rebuild the embedded analyze dashboard and re-stamp it.
##
## `pnpm build` is enough: ui-analyze/vite.config.js sets `outDir` to the
## crate-root ui-analyze-dist/ and runs the stamp plugin on closeBundle.
analyze-ui:
	@if command -v pnpm > /dev/null 2>&1; then \
	  echo ">> pnpm install && pnpm build (in $(ANALYZE_UI))"; \
	  cd $(ANALYZE_UI) && pnpm install --frozen-lockfile && pnpm build; \
	else \
	  echo ">> npm ci && npm run build (in $(ANALYZE_UI))"; \
	  cd $(ANALYZE_UI) && npm ci && npm run build; \
	fi

## Everything the committed bundles need before `cargo publish`.
##
## The console's own ui/dist/ is not rebuilt here: build.rs already rebuilds and
## re-stamps it whenever its source moves, and re-stamping it from a target that
## did not run the build would record a claim nobody made.
release-prep: search-ui memory-ui analyze-ui
	@echo ">> ui-search-dist, ui-memory-dist and ui-analyze-dist rebuilt and stamped. Ready for cargo publish."
