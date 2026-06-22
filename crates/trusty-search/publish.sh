#!/usr/bin/env bash
# publish.sh — Publish the trusty-search crate to crates.io from the monorepo.
#
# Why:
#   trusty-tools is a single Cargo workspace. The workspace-root `Cargo.toml`
#   carries a `[patch.crates-io]` block that redirects several in-tree library
#   crates (trusty-common, trusty-embedderd, trusty-bm25-daemon, trusty-console)
#   to local PATH dependencies so day-to-day workspace builds resolve them from
#   source. Those path patches MUST be stripped before `cargo publish` because
#   crates.io rejects path-patched dependencies — publish has to resolve every
#   dependency (including the `redb`/`redb2` 2.6+4.x pair used by the
#   `migrate-redb` subcommand) from the live registry, exactly as a downstream
#   consumer would. This script automates that strip → publish → restore cycle
#   safely, so the workspace-root manifest is always left untouched afterward.
#
# What:
#   1. Computes the repo root via `git rev-parse` (NOT the crate dir).
#   2. Temporarily removes the `[patch.crates-io]` block from the WORKSPACE-ROOT
#      Cargo.toml (idempotent backup + trap-based restore on ANY exit).
#   3. Runs `cargo publish -p trusty-search` (or `--dry-run`).
#   4. Restores the workspace-root Cargo.toml verbatim, even on failure.
#
# Test:
#   `bash crates/trusty-search/publish.sh --dry-run` from anywhere drives
#   `cargo publish -p trusty-search --dry-run` to a successful package/verify
#   (redb/redb2 resolve from crates.io) and leaves the root Cargo.toml unchanged
#   (`git status --porcelain Cargo.toml` is empty afterward). A dry-run needs no
#   crates.io token.
#
# PREREQUISITES (real publish only — dry-run needs none of these):
#   1. `cargo login <token>` has been run (verified crates.io account).
#   2. Every workspace lib that trusty-search depends on (e.g. trusty-common,
#      trusty-embedderd) is ALREADY published at the version trusty-search pins.
#      See `.claude/skills/cargo-publish/SKILL.md` for cross-crate ordering.
#
# USAGE:
#   bash crates/trusty-search/publish.sh [--dry-run] [--help]
#     --dry-run   Package + verify without uploading (no token required).
#     (no flag)   Real publish to crates.io.

set -euo pipefail

CRATE="trusty-search"

usage() {
  sed -n '2,40p' "$0" | sed 's/^# \{0,1\}//'
  exit "${1:-0}"
}

DRY_RUN=0
for arg in "$@"; do
  case "$arg" in
    --dry-run) DRY_RUN=1 ;;
    -h|--help) usage 0 ;;
    *) echo "error: unknown argument: $arg" >&2; usage 1 ;;
  esac
done

log() { echo "[$(date +%H:%M:%S)] $*"; }

# ---------------------------------------------------------------------------
# Locate the workspace-root Cargo.toml (the ONLY place [patch.crates-io] lives).
# ---------------------------------------------------------------------------
REPO_ROOT="$(git rev-parse --show-toplevel)"
CARGO_TOML="$REPO_ROOT/Cargo.toml"
CARGO_TOML_BACKUP="$REPO_ROOT/Cargo.toml.prepublish-backup"
CARGO_LOCK="$REPO_ROOT/Cargo.lock"
CARGO_LOCK_BACKUP="$REPO_ROOT/Cargo.lock.prepublish-backup"

if [[ ! -f "$CARGO_TOML" ]]; then
  echo "error: workspace-root Cargo.toml not found at $CARGO_TOML" >&2
  exit 1
fi

# ---------------------------------------------------------------------------
# Strip / restore the [patch.crates-io] block.
#
# The block redirects in-tree libs to path deps; crates.io publish cannot
# resolve path patches. We restore the manifest verbatim on EVERY exit (success,
# failure, or Ctrl-C) via a trap, so the workspace is never left mutated.
#
# We back up Cargo.lock too: stripping the patch table forces cargo to re-resolve
# the in-tree libs from the registry, which rewrites Cargo.lock (e.g. annotating
# `trusty-common` with its explicit version). That rewrite is a publish-time
# artifact, not an intended source change, so we restore the original lockfile
# on exit as well — leaving the workspace exactly as we found it.
# ---------------------------------------------------------------------------
restore_workspace() {
  if [[ -f "$CARGO_TOML_BACKUP" ]]; then
    mv -f "$CARGO_TOML_BACKUP" "$CARGO_TOML"
    log "Restored workspace-root Cargo.toml ($CARGO_TOML)."
  fi
  if [[ -f "$CARGO_LOCK_BACKUP" ]]; then
    mv -f "$CARGO_LOCK_BACKUP" "$CARGO_LOCK"
    log "Restored workspace-root Cargo.lock ($CARGO_LOCK)."
  fi
}
trap restore_workspace EXIT INT TERM

log "Backing up workspace-root Cargo.toml and Cargo.lock..."
cp "$CARGO_TOML" "$CARGO_TOML_BACKUP"
[[ -f "$CARGO_LOCK" ]] && cp "$CARGO_LOCK" "$CARGO_LOCK_BACKUP"

log "Stripping [patch.crates-io] block from workspace-root Cargo.toml for publish..."
# Remove the [patch.crates-io] table (and the comment lines immediately above it)
# up to the next top-level table header or EOF. This drops BOTH the in-tree path
# patches AND any future redb/redb2 patch entries, so cargo resolves everything
# from crates.io. redb2 itself is a plain `package = "redb"` rename pinned to 2.6
# and needs no patch entry — it resolves from the registry directly.
python3 - "$CARGO_TOML" <<'PYEOF'
import sys, re

path = sys.argv[1]
text = open(path).read()

# Match an optional run of comment lines directly above the [patch.crates-io]
# header, the header itself, and every line until the next top-level table
# header ("[...]" at column 0) or end-of-file.
pattern = re.compile(
    r'(?:^#.*\n)*^\[patch\.crates-io\]\n(?:^(?!\[).*\n?)*',
    flags=re.MULTILINE,
)
new_text, n = pattern.subn('', text)
if n == 0:
    print("WARNING: no [patch.crates-io] block found — nothing stripped.", file=sys.stderr)
else:
    open(path, 'w').write(new_text)
    print(f"Stripped {n} [patch.crates-io] block(s).")
PYEOF

# ---------------------------------------------------------------------------
# Publish (or dry-run).
# ---------------------------------------------------------------------------
# `--allow-dirty` is REQUIRED here: stripping [patch.crates-io] from the
# workspace-root Cargo.toml leaves an uncommitted edit, which `cargo publish`
# otherwise refuses ("files in the working directory contain changes"). The edit
# is transient — the EXIT trap restores the manifest the instant publish returns
# — so allowing it is safe. The stripped patch table is workspace-build-only
# metadata and never belongs in the published tarball regardless.
PUBLISH_ARGS=(publish -p "$CRATE" --allow-dirty)
if [[ "$DRY_RUN" -eq 1 ]]; then
  PUBLISH_ARGS+=(--dry-run)
  log "Running DRY-RUN: cargo ${PUBLISH_ARGS[*]} (no upload, no token required)..."
else
  log "Publishing $CRATE to crates.io: cargo ${PUBLISH_ARGS[*]} ..."
fi

# NOTE: we deliberately do NOT pass `--locked`. Stripping [patch.crates-io]
# changes how the in-tree libs resolve (path patch → crates.io registry), which
# legitimately rewrites Cargo.lock; `--locked` would abort that with
# "cannot update the lock file ... because --locked was passed". Letting cargo
# regenerate the lock against the registry is exactly what a downstream consumer
# sees, so it is the correct publish-time resolution.
# SKIP_UI_BUILD=1 avoids re-running the Svelte build in build.rs (the embedded
# ui-dist/ is already committed; see crate CLAUDE.md).
SKIP_UI_BUILD="${SKIP_UI_BUILD:-1}" cargo "${PUBLISH_ARGS[@]}"

# Restore happens via the EXIT trap.
if [[ "$DRY_RUN" -eq 1 ]]; then
  log "Dry-run complete. No crate was uploaded."
else
  log "Done! Published: https://crates.io/crates/$CRATE"
fi
