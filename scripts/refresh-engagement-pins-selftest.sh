#!/usr/bin/env bash
#
# refresh-engagement-pins-selftest.sh — fixtures for
# scripts/refresh-engagement-pins.sh.
#
# Why: the gate this drives is the only mechanical answer to #6772, and its
#   failing branches are the half that matters — a gate that always exits 0
#   looks identical to one that checks something, which is how three stale
#   pins shipped compiled into trusty-audit at 7cfeda52d with every other
#   release check green.
#
# What: builds synthetic single-file cargo workspaces under a scratch
#   directory, each with its own `crates/trusty-audit/templates/
#   engagement.template.toml`, and runs the gate against them with `--repo`.
#   Asserts exit status and output for both modes, that a rewrite is
#   idempotent to the byte, that the commented `# [tools]` digest example is
#   never rewritten, and that an unreadable table fails closed. The last case
#   runs `--check` against the REAL checked-out template.
#
# Test: this IS the test. Run directly:
#   bash scripts/refresh-engagement-pins-selftest.sh
#
# Portability: POSIX tools plus cargo and python3, bash 3.2 (macOS) and
#   bash 5 (Linux CI).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)"
GATE="${SCRIPT_DIR}/refresh-engagement-pins.sh"

PASSED=0
FAILED=0
WORK="$(mktemp -d "${TMPDIR:-/tmp}/refresh-engagement-pins-selftest.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

pass_case() { echo "  ok  $1"; PASSED=$((PASSED + 1)); }
fail_case() {
  echo "SELF-TEST FAIL: $1" >&2
  shift
  printf '%s\n' "$@" | sed 's/^/       /' >&2
  FAILED=$((FAILED + 1))
}

# mkrepo <name> <pkg=version>... -> prints the repo path. A minimal cargo
# workspace: enough for `cargo metadata --no-deps` to report each package's
# version, plus the template directory the gate reads. No external
# dependencies, so metadata resolves offline in milliseconds.
mkrepo() {
  local name="$1"; shift
  local repo="${WORK}/${name}" spec pkg version i=0
  mkdir -p "${repo}/crates/trusty-audit/templates"
  printf '[workspace]\nmembers = ["crates/*"]\nresolver = "2"\n' > "${repo}/Cargo.toml"
  printf '[package]\nname = "trusty-audit"\nversion = "0.14.0"\nedition = "2021"\n' \
    > "${repo}/crates/trusty-audit/Cargo.toml"
  mkdir -p "${repo}/crates/trusty-audit/src"
  : > "${repo}/crates/trusty-audit/src/lib.rs"
  for spec in "$@"; do
    pkg="${spec%%=*}"
    version="${spec#*=}"
    i=$((i + 1))
    mkdir -p "${repo}/crates/dep${i}/src"
    printf '[package]\nname = "%s"\nversion = "%s"\nedition = "2021"\n' "$pkg" "$version" \
      > "${repo}/crates/dep${i}/Cargo.toml"
    : > "${repo}/crates/dep${i}/src/lib.rs"
  done
  echo "$repo"
}

template_path() { echo "$1/crates/trusty-audit/templates/engagement.template.toml"; }

# run_case <label> <expect-exit> <expect-substring|-> <repo> [gate args...]
run_case() {
  local label="$1" want_exit="$2" want_sub="$3" repo="$4"
  shift 4
  local out rc=0
  out="$(bash "$GATE" --repo "$repo" "$@" 2>&1)" || rc=$?
  if [ "$rc" -ne "$want_exit" ]; then
    fail_case "${label}: expected exit ${want_exit}, got ${rc}" "$out"
    return
  fi
  if [ "$want_sub" != "-" ] && ! grep -qF -- "$want_sub" <<< "$out"; then
    fail_case "${label}: exit ${rc} but output never said '${want_sub}'" "$out"
    return
  fi
  pass_case "${label} -> exit ${rc}${want_sub:+, reported ${want_sub}}"
}

# ===========================================================================
# 1. Every pin already at its workspace version. --check exits 0, and the
#    default mode leaves the file byte-identical.
# ===========================================================================
repo="$(mkrepo current tga=7.1.0 trusty-review=0.34.0)"
cat > "$(template_path "$repo")" <<'EOF'
client = "Acme"

[tools]
tga = "7.1.0"
trusty-review = "0.34.0"

[report]
template = "cast"
EOF
before="$(cat "$(template_path "$repo")")"
run_case "current pins: --check" 0 "OK" "$repo" --check
run_case "current pins: rewrite" 0 "no change" "$repo"
if [ "$before" = "$(cat "$(template_path "$repo")")" ]; then
  pass_case "current pins: rewrite left the file byte-identical"
else
  fail_case "current pins: rewrite modified an already-current template" \
    "$(diff <(printf '%s\n' "$before") "$(template_path "$repo")" || true)"
fi

# ===========================================================================
# 2. THE REGRESSION — the #6772 shape. Two pins lag the workspace versions
#    they are about to ship beside. --check names each one and exits 1; the
#    rewrite fixes exactly those and a re-check goes clean.
# ===========================================================================
repo="$(mkrepo stale tga=7.1.0 trusty-search=0.52.0 trusty-review=0.34.0)"
cat > "$(template_path "$repo")" <<'EOF'
[tools]
tga = "6.0.0"
trusty-search = "0.52.0"
trusty-review = "0.33.0"
EOF
run_case "stale pins: --check names tga" 1 "STALE tga pinned=6.0.0 workspace=7.1.0" "$repo" --check
run_case "stale pins: --check names trusty-review" 1 \
  "STALE trusty-review pinned=0.33.0 workspace=0.34.0" "$repo" --check
out="$(bash "$GATE" --repo "$repo" --check 2>&1)" || true
if grep -qF "STALE trusty-search" <<< "$out"; then
  fail_case "stale pins: --check falsely reported the already-current trusty-search" "$out"
else
  pass_case "stale pins: --check left the already-current trusty-search alone"
fi
run_case "stale pins: rewrite" 0 "tga: 6.0.0 -> 7.1.0" "$repo"
run_case "stale pins: --check after rewrite" 0 "OK" "$repo" --check

# ===========================================================================
# 3. Idempotence — a second rewrite over the just-refreshed template reports
#    no change and produces the same bytes.
# ===========================================================================
after_first="$(cat "$(template_path "$repo")")"
run_case "idempotence: second rewrite" 0 "no change" "$repo"
if [ "$after_first" = "$(cat "$(template_path "$repo")")" ]; then
  pass_case "idempotence: second rewrite left the file byte-identical"
else
  fail_case "idempotence: second rewrite changed the file" \
    "$(diff <(printf '%s\n' "$after_first") "$(template_path "$repo")" || true)"
fi

# ===========================================================================
# 4. The inline-table digest spelling the template documents. The version is
#    refreshed; the sha256 beside it survives untouched.
# ===========================================================================
repo="$(mkrepo digest trusty-review=0.34.0)"
cat > "$(template_path "$repo")" <<'EOF'
[tools]
trusty-review = { version = "0.32.0", sha256 = "deadbeef" }
EOF
run_case "digest spelling: --check" 1 "STALE trusty-review pinned=0.32.0 workspace=0.34.0" \
  "$repo" --check
run_case "digest spelling: rewrite" 0 "0.32.0 -> 0.34.0" "$repo"
if grep -qF 'trusty-review = { version = "0.34.0", sha256 = "deadbeef" }' "$(template_path "$repo")"; then
  pass_case "digest spelling: version refreshed, sha256 preserved"
else
  fail_case "digest spelling: rewrite mangled the inline table" "$(cat "$(template_path "$repo")")"
fi

# ===========================================================================
# 5. The commented `# [tools]` example further down the file is documentation,
#    not a pin — a rewrite must never touch it, and it must never be read as a
#    pin either.
# ===========================================================================
repo="$(mkrepo commented tga=7.1.0)"
cat > "$(template_path "$repo")" <<'EOF'
[tools]
tga = "6.0.0"

# [tools]
# tga = "1.0.0"
# trusty-review = { version = "0.32.0", sha256 = "…" }
EOF
run_case "commented example: rewrite" 0 "tga: 6.0.0 -> 7.1.0" "$repo"
if grep -qF '# tga = "1.0.0"' "$(template_path "$repo")"; then
  pass_case "commented example: left verbatim"
else
  fail_case "commented example: the rewrite edited a comment" "$(cat "$(template_path "$repo")")"
fi

# ===========================================================================
# 6. Fail-closed cases. Each of these would otherwise report success over a
#    table the gate never actually read.
# ===========================================================================
repo="$(mkrepo unknown tga=7.1.0)"
cat > "$(template_path "$repo")" <<'EOF'
[tools]
tga = "7.1.0"
not-a-workspace-crate = "1.0.0"
EOF
run_case "unknown package pin" 2 "not-a-workspace-crate" "$repo" --check

repo="$(mkrepo emptytable tga=7.1.0)"
cat > "$(template_path "$repo")" <<'EOF'
[tools]

[report]
template = "cast"
EOF
run_case "empty [tools] table" 2 "no readable pins" "$repo" --check

repo="$(mkrepo notable tga=7.1.0)"
cat > "$(template_path "$repo")" <<'EOF'
client = "Acme"

[report]
template = "cast"
EOF
run_case "missing [tools] table" 2 "no readable pins" "$repo" --check

repo="$(mkrepo notemplate tga=7.1.0)"
rm -f "$(template_path "$repo")"
run_case "missing template file" 2 "no template at" "$repo" --check

# ===========================================================================
# 7. THE LIVE REPO. Proves the checked-out template is current, rather than
#    only the fixtures built to make the gate pass.
# ===========================================================================
run_case "live repo: checked-out template" 0 "OK" "$REPO_ROOT" --check

echo
echo "refresh-engagement-pins-selftest: ${PASSED} passed, ${FAILED} failed."
[ "$FAILED" -eq 0 ]
