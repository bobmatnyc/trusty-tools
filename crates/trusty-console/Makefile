# trusty-console Makefile
#
# Why: this crate packages TWO committed UI bundles, and build.rs keeps both
# current. `ui/dist/` is the console's own management app; `ui-search-dist/` is
# the search dashboard it serves at /tools/search/ (#6155). Until #6155 that
# second bundle was built from crates/trusty-search/ui — another crate's source,
# which build.rs deliberately did not reach into. The source now lives in this
# crate at `ui-search/`, so build.rs builds it like any other, and these targets
# exist for the two callers that ask for a rebuild by name: an operator
# preparing a release, and check-ui-bundle-freshness.sh's remedy line.
# What: `search-ui` rebuilds `ui-search-dist/` from `ui-search/`; the Vite
# config writes to that directory and re-stamps it, so no mirror step follows.
# `release-prep` is the name check-ui-bundle-freshness.sh names in its remedy.
# Test: after `make -C crates/trusty-console release-prep`,
# `bash scripts/check-ui-bundle-freshness.sh trusty-console` passes both rows.

MAKEFILE_DIR := $(dir $(abspath $(lastword $(MAKEFILE_LIST))))
REPO_ROOT    := $(MAKEFILE_DIR)../..
SEARCH_UI    := $(MAKEFILE_DIR)ui-search

.PHONY: search-ui release-prep

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

## Everything the committed bundles need before `cargo publish`.
##
## The console's own ui/dist/ is not rebuilt here: build.rs already rebuilds and
## re-stamps it whenever its source moves, and re-stamping it from a target that
## did not run the build would record a claim nobody made.
release-prep: search-ui
	@echo ">> ui-search-dist rebuilt and stamped. Ready for cargo publish."
