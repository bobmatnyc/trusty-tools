#!/usr/bin/env bash
# scripts/check_buildrs_watched_paths.sh
#
# Why: cargo treats a `rerun-if-changed` path that does not exist as changed,
# so a build script that watches a missing path re-runs on every build and
# rebuilds its crate and every dependent. trusty-search watched a `ui/` tree
# that #6155 deleted, and nothing caught it (QUICK BUILDS, 2026-09-27).
#
# What: finds every build script (a `build.rs` next to a `Cargo.toml` under
# crates/) and extracts two emit shapes:
#   - a literal `cargo:rerun-if-changed=<path>` (either colon form);
#   - a `for <var> in [ "...", ... ]` array whose body prints
#     `rerun-if-changed={<var>}`. The loop is GUARDED when its body calls
#     `.exists()` before the print, and then a missing entry is legitimate.
# Each unguarded path is resolved against the package root and must exist in a
# clean checkout: a tracked file, or a directory holding one (`git ls-files`).
# Outside a git work tree the check falls back to the filesystem. A path built
# with `{}` and a runtime value is reported as dynamic and not checked.
#
# Usage: check_buildrs_watched_paths.sh [--list] [ROOT]
#   --list  print every watched path with its status (the survey table)
#   ROOT    workspace root; defaults to this script's parent directory
# Exit: 0 when every unguarded watched path exists, 1 otherwise.
#
# Test: scripts/check_buildrs_watched_paths_selftest.sh; CI runs it in
# .github/workflows/buildrs-sync.yml before scripts/check_buildrs_sync.sh,
# which calls this script.

set -euo pipefail

LIST=0
ROOT=""
for arg in "$@"; do
    case "$arg" in
        --list) LIST=1 ;;
        -*) echo "unknown option: $arg" >&2; exit 2 ;;
        *) ROOT="$arg" ;;
    esac
done
[[ -n "$ROOT" ]] || ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ROOT="$(cd "$ROOT" && pwd)"

# Known-missing watches another open PR removes. Row: `<build.rs>|<path>|<why>`.
# A row that no longer matches anything only warns, so the PR that fixes the
# path is not blocked by this list.
EXEMPT=(
    "crates/trusty-agents/build.rs|.git/HEAD|#8808 replaces it with the resolved git dir"
    "crates/trusty-agents/build.rs|.git/index|#8808 replaces it with the resolved git dir"
)

IN_GIT=0
if git -C "$ROOT" rev-parse --is-inside-work-tree > /dev/null 2>&1; then
    IN_GIT=1
fi

# Print `<kind>\t<path>` per watched path in the build script on stdin, where
# kind is `plain`, `guarded` or `dynamic`. Comment lines are dropped first so
# prose that quotes a directive is not read as one.
extract() {
    perl -0777 -ne '
        s{^[ \t]*//[^\n]*}{}mg;
        while (/cargo::?rerun-if-changed=([^"]*)"/g) {
            my $p = $1;
            print(($p =~ /\{/ ? "dynamic" : "plain"), "\t$p\n")
                unless $p =~ /^\{(\w+)\}$/;
        }
        while (/for\s+(\w+)\s+in\s+\[([^\]]*)\]\s*\{/g) {
            my ($var, $items, $at) = ($1, $2, pos($_));
            my $body = substr($_, $at, 600);
            my $emit = index($body, "rerun-if-changed={$var}");
            next if $emit < 0;
            my $kind = (substr($body, 0, $emit) =~ /\.exists\(\)/) ? "guarded" : "plain";
            print "$kind\t$1\n" while $items =~ /"([^"]+)"/g;
        }
    '
}

# True when `rel`, relative to package dir `dir`, exists in a clean checkout.
exists_clean() {
    local dir="$1" rel="$2"
    if [[ "$IN_GIT" == 1 ]]; then
        [[ -n "$(git -C "$dir" ls-files -- "$rel" 2> /dev/null | head -n 1)" ]]
    else
        [[ -e "$dir/$rel" ]]
    fi
}

is_exempt() {
    local row
    for row in "${EXEMPT[@]}"; do
        [[ "${row%%|*}" == "$1" && "$(echo "$row" | cut -d'|' -f2)" == "$2" ]] && return 0
    done
    return 1
}

FAILED=0
SCRIPTS=0
USED_EXEMPT=""
while IFS= read -r buildrs; do
    dir="$(dirname "$buildrs")"
    [[ -f "$dir/Cargo.toml" ]] || continue
    SCRIPTS=$((SCRIPTS + 1))
    rel_script="${buildrs#"$ROOT"/}"
    while IFS="$(printf '\t')" read -r kind path; do
        [[ -n "$kind" ]] || continue
        if [[ "$kind" == dynamic ]]; then
            status="dynamic"
        elif exists_clean "$dir" "$path"; then
            status="exists"
        elif [[ "$kind" == guarded ]]; then
            status="missing-guarded"
        elif is_exempt "$rel_script" "$path"; then
            status="missing-exempt"
            USED_EXEMPT="$USED_EXEMPT|$rel_script|$path|"
        else
            status="MISSING"
            FAILED=1
            echo "FAIL: $rel_script watches \`$path\`, which does not exist in a clean checkout." >&2
        fi
        if [[ "$LIST" == 1 ]]; then
            printf '%s\t%s\t%s\n' "$rel_script" "$path" "$status"
        fi
    done < <(extract < "$buildrs")
done < <(find "$ROOT/crates" \( -name node_modules -o -name 'target*' -o -name .git \) -prune \
    -o -name build.rs -type f -print | LC_ALL=C sort)

for row in "${EXEMPT[@]}"; do
    key="|$(echo "$row" | cut -d'|' -f1-2)|"
    [[ "$USED_EXEMPT" == *"$key"* ]] ||
        echo "WARN: stale exemption, remove it from $0: $row" >&2
done

if [[ "$SCRIPTS" == 0 ]]; then
    echo "FAIL: found no build scripts under $ROOT/crates; refusing an empty scan." >&2
    exit 1
fi
if [[ "$FAILED" == 1 ]]; then
    echo "To fix: drop the watch, or guard it with \`.exists()\` if the path is optional." >&2
    exit 1
fi
echo "watched paths: every unguarded rerun-if-changed path exists across $SCRIPTS build scripts."
