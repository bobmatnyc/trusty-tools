# trusty-console Makefile
#
# Why: this crate packages TWO committed UI bundles. `ui/dist/` is its own
# Svelte app and build.rs keeps it current. `ui-search-dist/` is the
# trusty-search SPA the console serves at /tools/search/ (#6155), built from
# crates/trusty-search/ui — another crate's source, which build.rs deliberately
# does not reach into: that would make every `cargo build -p trusty-console`
# install and build a second Node project, and would write into a tree this
# crate does not own. So that bundle is a committed artefact refreshed by an
# explicit command, the same discipline trusty-search uses for its own
# `ui-dist/`.
# What: `search-ui` rebuilds `ui-search-dist/` from the trusty-search UI source
# and re-records the freshness stamp. `release-prep` is the name
# check-ui-bundle-freshness.sh names in its remedy line.
# Test: after `make -C crates/trusty-console release-prep`,
# `bash scripts/check-ui-bundle-freshness.sh trusty-console` passes both rows.

MAKEFILE_DIR := $(dir $(abspath $(lastword $(MAKEFILE_LIST))))
REPO_ROOT    := $(MAKEFILE_DIR)../..
SEARCH_UI    := $(REPO_ROOT)/crates/trusty-search/ui
SEARCH_EMBED := $(MAKEFILE_DIR)ui-search-dist

.PHONY: search-ui release-prep

## Rebuild the embedded trusty-search SPA and re-stamp it.
##
## `vite build --outDir` writes straight into this crate, so nothing is left
## behind in crates/trusty-search/. `--emptyOutDir` is required because the
## target sits outside the Vite project root.
search-ui:
	@if command -v pnpm > /dev/null 2>&1; then \
	  echo ">> pnpm install && vite build -> $(SEARCH_EMBED)"; \
	  cd $(SEARCH_UI) && pnpm install --frozen-lockfile \
	    && pnpm exec vite build --outDir $(SEARCH_EMBED) --emptyOutDir; \
	else \
	  echo ">> npm ci && vite build -> $(SEARCH_EMBED)"; \
	  cd $(SEARCH_UI) && npm ci \
	    && npx vite build --outDir $(SEARCH_EMBED) --emptyOutDir; \
	fi
	@bash $(REPO_ROOT)/scripts/stamp-ui-bundle.sh trusty-console-search

## Everything the committed bundles need before `cargo publish`.
##
## The console's own ui/dist/ is not rebuilt here: build.rs already rebuilds and
## re-stamps it whenever its source moves, and re-stamping it from a target that
## did not run the build would record a claim nobody made.
release-prep: search-ui
	@echo ">> ui-search-dist rebuilt and stamped. Ready for cargo publish."
