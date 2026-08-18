#!/usr/bin/env bash
#
# ui_source_digest.sh — the one definition of "which UI source, and what does
# it hash to" (#3606).
#
# Why: two things need this answer and they must never disagree — the stamper
#   (scripts/stamp-ui-bundle.sh) writes the digest into the bundle, and the gate
#   (scripts/check-ui-bundle-freshness.sh) recomputes it and compares. Two
#   implementations of the same rule would drift, and the drift would present as
#   a permanently-failing gate whose remedy does not clear it.
#
# What: the digest is git's own hash of a canonical record set — one
#   "<blob-sha>  <path>" line per bundle-affecting source file, sorted by path.
#   Blob SHAs are content hashes, so the digest changes when and only when the
#   source content changes. Two sources of those SHAs, producing identical
#   output for identical content:
#     --worktree   files on disk (tracked + untracked-not-ignored), hashed with
#                  `git hash-object`. Used when stamping a fresh build and when
#                  the gate runs against a working tree, so an uncommitted edit
#                  counts.
#     <rev>        blob SHAs read straight out of `git ls-tree -r <rev>`.
#
#   Bundle-affecting means everything tracked under the UI project except the
#   bundle directory itself, vendored dependencies, and prose. Prose is excluded
#   so a README edit does not demand a rebuild.
#
# Test: scripts/check-ui-bundle-freshness-selftest.sh — case 3 (a source change
#   must move the digest) and case 20 (an edit under the bundle directory must
#   NOT move it, which is the laundering case).
#
# Portability: POSIX tools only, bash 3.2 (macOS) and bash 5 (Linux CI).

# ui_source_paths <repo> <rev|--worktree> <src_dir> <bundle_dir>
# Prints repo-relative paths, one per line, unsorted.
ui_source_paths() {
  local repo="$1" rev="$2" src_dir="$3" bundle_dir="$4"
  local raw
  if [ "$rev" = "--worktree" ]; then
    # Untracked-but-not-ignored files are included: a new .svelte that has not
    # been `git add`ed still changes what a build would produce.
    raw="$( {
      git -C "$repo" ls-files -- "$src_dir"
      git -C "$repo" ls-files --others --exclude-standard -- "$src_dir"
    } | sort -u)"
  else
    raw="$(git -C "$repo" ls-tree -r --name-only "$rev" -- "$src_dir" || true)"
  fi
  printf '%s\n' "$raw" \
    | grep -v "^${bundle_dir}/" \
    | grep -v '/node_modules/' \
    | grep -v '\.md$' \
    | grep -v '^[[:space:]]*$' || true
}

# ui_source_digest <repo> <rev|--worktree> <src_dir> <bundle_dir>
# Prints "<digest> <file-count>". Returns 1 when there is nothing to hash —
# callers must treat that as a failure, never as an empty-but-fine digest.
ui_source_digest() {
  local repo="$1" rev="$2" src_dir="$3" bundle_dir="$4"
  local paths count records digest
  paths="$(ui_source_paths "$repo" "$rev" "$src_dir" "$bundle_dir")"
  count="$(printf '%s\n' "$paths" | grep -c . || true)"
  [ "$count" -eq 0 ] && return 1

  if [ "$rev" = "--worktree" ]; then
    # A tracked-but-deleted file has no content to hash; dropping its record is
    # correct — the digest must change when a source file is removed.
    local existing
    existing="$(printf '%s\n' "$paths" | while IFS= read -r p; do
      [ -f "${repo}/${p}" ] && printf '%s\n' "$p"
    done)"
    count="$(printf '%s\n' "$existing" | grep -c . || true)"
    [ "$count" -eq 0 ] && return 1
    records="$(paste -d' ' \
      <(printf '%s\n' "$existing" | git -C "$repo" hash-object --stdin-paths) \
      <(printf '%s\n' "$existing") | sort)"
  else
    records="$(git -C "$repo" ls-tree -r "$rev" -- "$src_dir" \
      | awk -v b="${bundle_dir}/" '
          { path = $0; sub(/^[^\t]*\t/, "", path)
            if (index(path, b) == 1) next
            if (path ~ /\/node_modules\//) next
            if (path ~ /\.md$/) next
            print $3 " " path }' \
      | sort)"
    count="$(printf '%s\n' "$records" | grep -c . || true)"
    [ "$count" -eq 0 ] && return 1
  fi

  digest="$(printf '%s\n' "$records" | git -C "$repo" hash-object --stdin)"
  [ -z "$digest" ] && return 1
  echo "${digest} ${count}"
}

# ui_stamp_path <bundle_dir> — where the recorded digest lives.
#
# Inside the bundle so it travels with it: `sync-ui` mirrors the directory,
# trusty-search's Cargo.toml `include` ships `ui-dist/**/*`, so the published
# tarball records what it was built from and stays checkable after the fact.
# Deliberately not a dotfile — cargo's include globs and `cp -r` treat hidden
# entries inconsistently enough that it is not worth finding out.
ui_stamp_path() {
  echo "$1/ui-source-hash.txt"
}

# ui_read_stamp <file> — prints the digest recorded in a stamp file, or fails.
# The first non-comment, non-blank line's first field.
ui_read_stamp() {
  local content="$1" digest
  digest="$(printf '%s\n' "$content" \
    | grep -v '^[[:space:]]*#' \
    | grep -v '^[[:space:]]*$' \
    | head -1 \
    | awk '{ print $1 }')"
  [ -z "$digest" ] && return 1
  echo "$digest"
}
