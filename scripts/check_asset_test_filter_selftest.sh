#!/usr/bin/env bash
#
# check_asset_test_filter_selftest.sh — fixtures for the asset-content test
#   list guard (#8378).
#
# Why: the guard is what keeps scripts/asset-content-tests.tsv complete. If it
#   stops seeing an asset read, a new asset-reading test runs nowhere on an
#   asset-only diff and the guard still reports green.
# What: builds a fixture workspace and asserts the guard
#   - passes when every read is listed, and ignores tests that read nothing
#     (a comment naming src/assets, a `src/assets` path in a crate without
#     that directory, an unrelated same-named constant);
#   - fails for each read shape planted outside the list: include_str! of an
#     asset, a `src/assets` literal, a qualified asset const, a bare const
#     under `use super::*`, a loader fn, an integration-test target, and a
#     `#[path]` test module;
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
  mkdir -p "$root/crates/fix/src/assets/skills" "$root/crates/fix/src/core" "$root/crates/fix/tests" \
    "$root/crates/other/src" "$root/scripts"
  printf '[package]\nname = "fix"\nversion = "0.0.0"\n' > "$root/crates/fix/Cargo.toml"
  printf '[package]\nname = "other"\nversion = "0.0.0"\n' > "$root/crates/other/Cargo.toml"
  echo "# skill" > "$root/crates/fix/src/assets/skills/a.md"
  cat > "$root/crates/fix/src/lib.rs" <<'EOF'
pub mod core;
EOF
  cat > "$root/crates/fix/src/core/mod.rs" <<'EOF'
pub mod bundle;
EOF
  cat > "$root/crates/fix/src/core/bundle.rs" <<'EOF'
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
    // Mentions crates/fix/src/assets/skills/a.md in a comment only.
    #[test]
    fn web_bundle_path() {
        assert_eq!("src/assets/main-AbCd.js".len(), 23);
        assert_eq!(super::ALL, 1);
    }
}
EOF
  printf 'fix\tlib\tcore::bundle::tests\treads SKILL_A\n' > "$root/scripts/list.tsv"
}

# plant <root> <file under crates/fix> <content>: append a test module.
plant() {
  mkdir -p "$(dirname "$1/crates/fix/$2")"
  printf '%s\n' "$3" >> "$1/crates/fix/$2"
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
awk -F'\t' '$1 == "trusty-agents-common"' "$REPO_ROOT/scripts/asset-content-tests.tsv" > "$R/scripts/list.tsv"
check "copy of trusty-agents-common, unmodified" 0 "$R" "$R/scripts/list.tsv"
printf '\n#[cfg(test)]\nmod planted_8378 {\n    #[test]\n    fn reads_base_ops() {\n        assert!(!crate::agent_assets::BASE_OPS.is_empty());\n    }\n}\n' \
  >> "$R/crates/trusty-agents-common/src/lib.rs"
check "planted reader in the real trusty-agents-common" 1 "$R" "$R/scripts/list.tsv" \
  "trusty-agents-common lib planted_8378::reads_base_ops"
check "the real tree and scripts/asset-content-tests.tsv" 0 "$REPO_ROOT" "$REPO_ROOT/scripts/asset-content-tests.tsv"

echo
echo "check_asset_test_filter selftest: ${CASES} cases, ${FAILURES} failure(s)"
[ "$FAILURES" -eq 0 ]
