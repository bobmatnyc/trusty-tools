#!/usr/bin/env bash
#
# check_asset_test_filter_selftest.sh — fixtures for the asset-content test
#   list guard (#8378).
#
# Why: the guard is what keeps scripts/asset-content-tests.tsv complete. If it
#   stops seeing an asset read, a new asset-reading test runs nowhere on an
#   asset-only diff and the guard still reports green.
# What: builds a fixture workspace — package `fix` in crates/trusty-mpm, whose
#   src/assets .md files are Cargo-inert — and asserts the guard
#   - passes when every read is listed, and ignores tests that read nothing
#     (a comment naming src/assets, a `src/assets` path in a crate without
#     that directory, an unrelated same-named constant);
#   - fails for each read shape planted outside the list, each against a .md
#     asset: include_str!, a `src/assets` literal, a qualified asset const, a
#     bare const under `use super::*`, a loader fn, an integration-test
#     target, a `#[path]` test module, and a manifest naming the .md;
#   - passes with an unlisted reader of a non-.md asset (a hook script) and of
#     a compiled-in .md outside the inert roots (a trusty-code agent);
#   - fails on a malformed row and on a row naming no real target;
#   - plans one cargo invocation per crate + target with the rows' filters.
#   Then it plants a reader into a copy of the real trusty-agents-common crate
#   (the guard must see it), and runs the guard on the real tree (it must pass).
# Usage: ./scripts/check_asset_test_filter_selftest.sh
# Exit: 0 when every case matches; 1 otherwise, printing each mismatch.
# Test: this IS the test. capabilities-drift.yml runs it before the guard.

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
GUARD="${REPO_ROOT}/scripts/check_asset_test_filter.py"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/asset-filter-selftest.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

FAILURES=0
CASES=0

check() {
  CASES=$((CASES + 1))
  local label="$1" want="$2" root="$3" list="$4" pattern="${5:-}" out rc
  out="$(python3 "$GUARD" check --root "$root" --list "$list" 2>&1)"
  rc=$?
  if [ "$rc" != "$want" ]; then
    FAILURES=$((FAILURES + 1))
    echo "  FAIL: ${label}: expected exit ${want}, got ${rc}"
    printf '%s\n' "$out" | sed 's/^/        /'
  elif [ -n "$pattern" ] && ! printf '%s\n' "$out" | grep -qF -- "$pattern"; then
    FAILURES=$((FAILURES + 1))
    echo "  FAIL: ${label}: output does not name '${pattern}'"
    printf '%s\n' "$out" | sed 's/^/        /'
  else
    printf '  ok   %-58s -> exit %s\n' "$label" "$rc"
  fi
}

# ------------------------------------------------------------------ fixture
new_fixture() {
  local root="$1"
  mkdir -p "$root/crates/trusty-mpm/src/assets/skills" "$root/crates/trusty-mpm/src/core" "$root/crates/trusty-mpm/tests" \
    "$root/crates/other/src" "$root/scripts"
  printf '[package]\nname = "fix"\nversion = "0.0.0"\n' > "$root/crates/trusty-mpm/Cargo.toml"
  printf '[package]\nname = "other"\nversion = "0.0.0"\n' > "$root/crates/other/Cargo.toml"
  echo "# skill" > "$root/crates/trusty-mpm/src/assets/skills/a.md"
  cat > "$root/crates/trusty-mpm/src/lib.rs" <<'EOF'
pub mod core;
EOF
  cat > "$root/crates/trusty-mpm/src/core/mod.rs" <<'EOF'
pub mod bundle;
EOF
  cat > "$root/crates/trusty-mpm/src/core/bundle.rs" <<'EOF'
//! Embeds the skill.
pub const SKILL_A: &str = include_str!("../assets/skills/a.md");
pub const ALL: &[&str] = &[SKILL_A];

pub fn skill_names() -> Vec<&'static str> {
    ALL.to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listed_reader() {
        assert!(!SKILL_A.is_empty());
    }
}
EOF
  # `other` has no src/assets: its `src/assets/…` literal names a web path.
  cat > "$root/crates/other/src/lib.rs" <<'EOF'
pub const ALL: u8 = 1;

#[cfg(test)]
mod tests {
    // Mentions crates/trusty-mpm/src/assets/skills/a.md in a comment only.
    #[test]
    fn web_bundle_path() {
        assert_eq!("src/assets/main-AbCd.js".len(), 23);
        assert_eq!(super::ALL, 1);
    }
}
EOF
  printf 'fix\tlib\tcore::bundle::tests\treads SKILL_A\n' > "$root/scripts/list.tsv"
}

# plant <root> <file under crates/trusty-mpm> <content>: append a test module.
plant() {
  mkdir -p "$(dirname "$1/crates/trusty-mpm/$2")"
  printf '%s\n' "$3" >> "$1/crates/trusty-mpm/$2"
}

echo "fixture:"
F="$WORK/base"
new_fixture "$F"
check "every read listed; non-readers ignored" 0 "$F" "$F/scripts/list.tsv"

n=0
planted() {
  local label="$1" file="$2" body="$3" want_name="$4" setup="${5:-}"
  n=$((n + 1))
  local root="$WORK/p$n"
  new_fixture "$root"
  [ -n "$setup" ] && plant "$root" "$setup" ""
  plant "$root" "$file" "$body"
  check "planted: ${label}" 1 "$root" "$root/scripts/list.tsv" "$want_name"
}

planted "include_str! of an asset" "src/lib.rs" '
#[cfg(test)]
mod r1 {
    #[test]
    fn inc() { assert!(!include_str!("assets/skills/a.md").is_empty()); }
}' "fix lib r1::inc"

planted "src/assets literal" "src/lib.rs" '
#[cfg(test)]
mod r2 {
    #[test]
    fn lit() { let _ = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/assets/skills"); }
}' "fix lib r2::lit"

planted "qualified asset const" "src/lib.rs" '
#[cfg(test)]
mod r3 {
    #[test]
    fn qual() { assert!(!crate::core::bundle::ALL.is_empty()); }
}' "fix lib r3::qual"

planted "bare const under use super::*" "src/core/bundle.rs" '
#[cfg(test)]
mod more_tests {
    use super::*;
    #[test]
    fn bare() { assert_eq!(ALL.len(), 1); }
}' "fix lib core::bundle::more_tests::bare"

planted "loader fn" "src/lib.rs" '
#[cfg(test)]
mod r5 {
    use crate::core::bundle::skill_names;
    #[test]
    fn loader() { assert_eq!(skill_names().len(), 1); }
}' "fix lib r5::loader"

planted "integration-test target" "tests/e2e.rs" '
use fix::core::bundle;
const CATALOG: &[&str] = &[bundle::SKILL_A];
#[test]
fn e2e() { assert_eq!(CATALOG.len(), 1); }' "fix test:e2e (crate root)"

P="$WORK/path-module"
new_fixture "$P"
plant "$P" "src/core/bundle.rs" '
#[cfg(test)]
#[path = "bundle_extra_tests.rs"]
mod extra;'
plant "$P" "src/core/bundle_extra_tests.rs" '
use super::*;
#[test]
fn via_path() { assert!(!SKILL_A.is_empty()); }'
check "planted: #[path] test module" 1 "$P" "$P/scripts/list.tsv" "fix lib core::bundle::extra::via_path"

M="$WORK/manifest"
new_fixture "$M"
printf '{"file": "skills/a.md"}\n' > "$M/crates/trusty-mpm/src/assets/index.json"
plant "$M" "src/lib.rs" '
pub const INDEX: &str = include_str!("assets/index.json");
#[cfg(test)]
mod m {
    #[test]
    fn idx() { assert!(!super::INDEX.is_empty()); }
}'
check "planted: a manifest naming a .md asset" 1 "$M" "$M/scripts/list.tsv" "fix lib m::idx"

# #9011: the agent roster moved to the repo-root content/ tree; a literal
# naming it from a crate (`<manifest dir>/../../content/agents`) is a read.
C="$WORK/content-literal"
new_fixture "$C"
mkdir -p "$C/content/agents"
echo "# agent" > "$C/content/agents/qa.md"
plant "$C" "src/lib.rs" '
#[cfg(test)]
mod c {
    #[test]
    fn lit() { let _ = concat!(env!("CARGO_MANIFEST_DIR"), "/../../content/agents"); }
}'
check "planted: a repo-root content/ literal" 1 "$C" "$C/scripts/list.tsv" "fix lib c::lit"

# #9011 R4: a test-code helper over a run-time content loader is a loader;
# the TEST calling it is the reader, and the helper's module needs no row.
L="$WORK/content-loader"
new_fixture "$L"
plant "$L" "src/lib.rs" '
#[cfg(test)]
mod support {
    pub fn roster() { let _ = AgentRoster::load(); }
}
#[cfg(test)]
mod uses {
    #[test]
    fn calls() { super::support::roster(); }
}'
check "planted: a test helper over AgentRoster::load" 1 "$L" "$L/scripts/list.tsv" "fix lib uses::calls"
{ cat "$L/scripts/list.tsv"; printf 'fix\tlib\tuses\treads support::roster\n'; } > "$L/scripts/covered.tsv"
check "  ...and its caller's row alone covers it" 0 "$L" "$L/scripts/covered.tsv"

# #9011 R4: a same-named helper in another `tests` module is not that loader.
D="$WORK/content-loader-names"
new_fixture "$D"
plant "$D" "src/lib.rs" '
mod a {
    #[cfg(test)]
    mod tests {
        fn d() { let _ = checkout_content(); }
        use super::super::agent_content::checkout_content;
    }
}
mod b {
    #[cfg(test)]
    mod tests {
        fn d() {}
        #[test]
        fn calls_its_own_d() { d(); }
    }
}'
check "a same-named helper elsewhere is not a content read" 0 "$D" "$D/scripts/list.tsv"

echo "not asset-content readers (#8378 round 2b):"
N="$WORK/non-md"
new_fixture "$N"
mkdir -p "$N/crates/trusty-mpm/src/assets/hooks"
printf '#!/bin/sh\n' > "$N/crates/trusty-mpm/src/assets/hooks/pre-push"
plant "$N" "src/lib.rs" '
pub const HOOK: &str = include_str!("assets/hooks/pre-push");
#[cfg(test)]
mod hook_tests {
    #[test]
    fn hook() {
        assert!(!super::HOOK.is_empty());
        let _ = std::path::Path::new("src/assets/hooks");
    }
}'
check "an unlisted reader of a non-.md asset is not flagged" 0 "$N" "$N/scripts/list.tsv"

T="$WORK/compiled-md"
new_fixture "$T"
mkdir -p "$T/crates/trusty-code/src/assets/agents"
printf '[package]\nname = "tcode"\nversion = "0.0.0"\n' > "$T/crates/trusty-code/Cargo.toml"
echo "# agent" > "$T/crates/trusty-code/src/assets/agents/qa.md"
printf '%s\n' 'pub const QA: &str = include_str!("assets/agents/qa.md");
#[cfg(test)]
mod tests {
    #[test]
    fn qa() { assert!(!super::QA.is_empty()); }
}' > "$T/crates/trusty-code/src/lib.rs"
check "an unlisted reader of a compiled-in .md elsewhere is not flagged" 0 "$T" "$T/scripts/list.tsv"

echo "list rows:"
printf 'fix\tlib\tcore::bundle::tests\n' > "$F/scripts/bad.tsv"
check "row with three columns" 1 "$F" "$F/scripts/bad.tsv" "expected 4 tab-separated columns"
printf 'fix\tlib\tcore::bundle::tests\tok\nfix\ttest:nope\t*\tno such target\n' > "$F/scripts/bad.tsv"
check "row naming no real target" 1 "$F" "$F/scripts/bad.tsv" "no tested target test:nope"
printf 'fix\tlib\t*\twhole target\n' > "$F/scripts/star.tsv"
check "a whole-target row covers everything" 0 "$F" "$F/scripts/star.tsv"

CASES=$((CASES + 1))
printf 'fix\tlib\tcore::bundle::tests\ta\nfix\tlib\tcore::x::t\tb\nfix\ttest:e2e\t*\tc\n' > "$F/scripts/plan.tsv"
got="$(python3 "$GUARD" plan --root "$F" --list "$F/scripts/plan.tsv")"
want="cargo test -p fix --lib --locked --no-fail-fast -- core::bundle::tests core::x::t
cargo test -p fix --test e2e --locked --no-fail-fast --"
if [ "$got" = "$want" ]; then
  printf '  ok   %-58s\n' "plan groups rows per crate + target"
else
  FAILURES=$((FAILURES + 1))
  printf '  FAIL: plan: expected\n%s\n  got\n%s\n' "$want" "$got"
fi

echo "real tree:"
R="$WORK/real"
mkdir -p "$R/crates" "$R/scripts"
cp -R "$REPO_ROOT/crates/trusty-agents-common" "$R/crates/"
rm -rf "$R/crates/trusty-agents-common/target"
# #9011: the crate embeds its agents and harness docs from the repo-root content/.
cp -R "$REPO_ROOT/content" "$R/"
awk -F'\t' '$1 == "trusty-agents-common"' "$REPO_ROOT/scripts/asset-content-tests.tsv" > "$R/scripts/list.tsv"
check "copy of trusty-agents-common, unmodified" 0 "$R" "$R/scripts/list.tsv"
# #9011: the roster is read at run time; the planted test calls the loader.
printf '\n#[cfg(test)]\nmod planted_8378 {\n    #[test]\n    fn reads_the_roster() {\n        let _ = crate::agent_content::checkout_content(std::path::Path::new("."));\n    }\n}\n' \
  >> "$R/crates/trusty-agents-common/src/lib.rs"
check "planted reader in the real trusty-agents-common" 1 "$R" "$R/scripts/list.tsv" \
  "trusty-agents-common lib planted_8378::reads_the_roster"
check "the real tree and scripts/asset-content-tests.tsv" 0 "$REPO_ROOT" "$REPO_ROOT/scripts/asset-content-tests.tsv"

echo
echo "check_asset_test_filter selftest: ${CASES} cases, ${FAILURES} failure(s)"
[ "$FAILURES" -eq 0 ]
