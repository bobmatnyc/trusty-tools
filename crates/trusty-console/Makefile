# trusty-console Makefile
#
# Why: this crate packages THREE committed UI bundles, and build.rs keeps all
# three current. `ui/dist/` is the console's own management app;
# `ui-search-dist/` is the search dashboard it serves at /tools/search/ and
# `ui-memory-dist/` the memory dashboard at /tools/memory/ (#6155). Until #6155
# those two were built from another crate's source, which build.rs deliberately
# did not reach into. Both sources now live in this crate, so build.rs builds
# them like any other, and these targets exist for the two callers that ask for
# a rebuild by name: an operator preparing a release, and
# check-ui-bundle-freshness.sh's remedy line.
# What: `search-ui` rebuilds `ui-search-dist/` from `ui-search/` and `memory-ui`
# rebuilds `ui-memory-dist/` from `ui-memory/`; each Vite config writes to its
# own directory and re-stamps it, so no mirror step follows. `release-prep` is
# the name check-ui-bundle-freshness.sh names in its remedy.
# Test: after `make -C crates/trusty-console release-prep`,
# `bash scripts/check-ui-bundle-freshness.sh trusty-console` passes all three
# rows.

MAKEFILE_DIR := $(dir $(abspath $(lastword $(MAKEFILE_LIST))))
REPO_ROOT    := $(MAKEFILE_DIR)../..
SEARCH_UI    := $(MAKEFILE_DIR)ui-search
MEMORY_UI    := $(MAKEFILE_DIR)ui-memory

.PHONY: search-ui memory-ui release-prep

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

## Everything the committed bundles need before `cargo publish`.
##
## The console's own ui/dist/ is not rebuilt here: build.rs already rebuilds and
## re-stamps it whenever its source moves, and re-stamping it from a target that
## did not run the build would record a claim nobody made.
release-prep: search-ui memory-ui
	@echo ">> ui-search-dist and ui-memory-dist rebuilt and stamped. Ready for cargo publish."
