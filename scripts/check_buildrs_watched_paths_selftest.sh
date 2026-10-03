#!/usr/bin/env bash
# The expected-output patterns quote the gate's literal backticks.
# shellcheck disable=SC2016
#
# check_buildrs_watched_paths_selftest.sh — fixture suite for
#   scripts/check_buildrs_watched_paths.sh (QUICK BUILDS, 2026-09-27).
#
# Why: the gate is worth something only if it FAILS on a build script that
#   watches a missing path. A gate that cannot be shown to fail silently stops
#   working (#4618). Each case drives the real script over a synthetic
#   workspace in a throwaway git repository, so no cargo and no build.
#
# What: nine cases, then the live repository scan must pass.
#   1. an unguarded literal watch of a missing path fails, naming the path;
#   2. the same watch passes once the path is committed;
#   3. an unguarded array loop with a missing entry fails;
#   4. an `.exists()`-guarded array loop with a missing entry passes;
#   5. a path on disk but not tracked (a build output) fails;
#   6. a commented-out directive is ignored;
#   7. a tree with no build scripts trips the scan floor;
#   8. an unreadable build.rs fails loudly instead of scanning as empty
#      (#8306-critic: a discarded process-substitution exit status used to
#      turn this into a silent pass);
#   9. perl missing from PATH fails before any build.rs is scanned, instead
#      of the same silent-EOF pass (#8306-critic).
#
# Usage: bash scripts/check_buildrs_watched_paths_selftest.sh
# Exit: 0 when every case behaves; 1 otherwise.
#
# Test: this file IS the test; CI runs it in .github/workflows/buildrs-sync.yml.
#
# Portability: bash 3.2 (macOS) and bash 5 (Linux CI).

# #7812: `zsh <this script>` has no BASH_SOURCE and 1-based arrays, so re-run
# under bash before any bash-only line. Plain POSIX, so zsh and sh parse it.
if [ -z "${BASH_VERSION:-}" ]; then exec bash "$0" "$@"; fi

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
GATE="${SCRIPT_DIR}/check_buildrs_watched_paths.sh"

WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT
OUT="${WORK}/out.txt"
failures=0

fail() {
    printf 'self-test FAIL: %s\n' "$1" >&2
    failures=$((failures + 1))
}

# new_ws <name> — an empty git-tracked workspace with one package `pkg`.
new_ws() {
    local ws="${WORK}/$1"
    mkdir -p "${ws}/crates/pkg/src"
    printf '[package]\nname = "pkg"\n' > "${ws}/crates/pkg/Cargo.toml"
    printf 'fn main() {}\n' > "${ws}/crates/pkg/src/lib.rs"
    git -C "${ws}" init -q
    echo "${ws}"
}

# track <ws> — stage every file so `git ls-files` sees it (a clean checkout).
track() { git -C "$1" add -A; }

# expect <exit> <label> <ws> [grep-pattern] — run the gate and check the result.
expect() {
    local want="$1" label="$2" ws="$3" pattern="${4:-}" got=0
    "${GATE}" "${ws}" > "${OUT}" 2>&1 || got=$?
    if [[ "${got}" != "${want}" ]]; then
        fail "${label}: exit ${got}, want ${want}"
        cat "${OUT}" >&2
    elif [[ -n "${pattern}" ]] && ! grep -q -- "${pattern}" "${OUT}"; then
        fail "${label}: output lacks '${pattern}'"
        cat "${OUT}" >&2
    fi
}

# expect_path <exit> <label> <ws> <path> <grep-pattern> — like expect, but runs
# the gate under a caller-built PATH (#8306-critic cases 8/9 need a farmed or
# perl-less PATH, never the ambient one).
expect_path() {
    local want="$1" label="$2" ws="$3" path="$4" pattern="$5" got=0
    PATH="${path}" "${GATE}" "${ws}" > "${OUT}" 2>&1 || got=$?
    if [[ "${got}" != "${want}" ]]; then
        fail "${label}: exit ${got}, want ${want}"
        cat "${OUT}" >&2
    elif [[ -n "${pattern}" ]] && ! grep -q -- "${pattern}" "${OUT}"; then
        fail "${label}: output lacks '${pattern}'"
        cat "${OUT}" >&2
    fi
}

# farm_path <dir> — a minimal PATH-worthy directory of symlinks to every
# external tool the gate needs EXCEPT perl, built from the ambient PATH so
# case 9 can prove "perl missing" fails fast rather than scanning as empty.
farm_path() {
    local dir="$1" tool p
    mkdir -p "${dir}"
    for tool in bash dirname git find sort cut head; do
        p="$(command -v "${tool}" 2> /dev/null || true)"
        [[ "${p}" == /* ]] && ln -sf "${p}" "${dir}/${tool}"
    done
}

# Case 1 + 2: literal watch, missing then committed.
ws="$(new_ws literal)"
printf 'fn main() {\n    println!("cargo:rerun-if-changed=ui/package.json");\n}\n' \
    > "${ws}/crates/pkg/build.rs"
track "${ws}"
expect 1 "missing literal watch" "${ws}" 'watches `ui/package.json`'
mkdir -p "${ws}/crates/pkg/ui" && echo '{}' > "${ws}/crates/pkg/ui/package.json"
track "${ws}"
expect 0 "committed literal watch" "${ws}"

# Case 3: unguarded loop with a missing entry.
ws="$(new_ws loop)"
cat > "${ws}/crates/pkg/build.rs" << 'RS'
fn main() {
    for rel in ["src", "ui/src"] {
        println!("cargo::rerun-if-changed={rel}");
    }
}
RS
track "${ws}"
expect 1 "unguarded loop" "${ws}" 'watches `ui/src`'

# Case 4: the same loop, guarded by `.exists()`.
cat > "${ws}/crates/pkg/build.rs" << 'RS'
fn main() {
    for rel in ["src", "ui/src"] {
        if std::path::Path::new(rel).exists() {
            println!("cargo::rerun-if-changed={rel}");
        }
    }
}
RS
track "${ws}"
expect 0 "guarded loop" "${ws}"

# Case 5: on disk but untracked — the build script's own output.
ws="$(new_ws untracked)"
printf 'fn main() {\n    println!("cargo:rerun-if-changed=ui/dist/index.html");\n}\n' \
    > "${ws}/crates/pkg/build.rs"
track "${ws}"
mkdir -p "${ws}/crates/pkg/ui/dist" && echo x > "${ws}/crates/pkg/ui/dist/index.html"
expect 1 "untracked build output" "${ws}" 'watches `ui/dist/index.html`'

# Case 6: a commented-out directive is prose, not a watch.
ws="$(new_ws comment)"
printf 'fn main() {\n    // println!("cargo:rerun-if-changed=gone.txt");\n}\n' \
    > "${ws}/crates/pkg/build.rs"
track "${ws}"
expect 0 "commented directive" "${ws}"

# Case 7: scan floor.
ws="$(new_ws empty)"
track "${ws}"
expect 1 "empty scan" "${ws}" 'refusing an empty scan'

# Case 8: an unreadable build.rs must FAIL loudly (#8306-critic) — the old
# gate fed it through a process substitution whose exit status bash discards,
# so a Permission-denied read produced silent EOF and a passing scan. Skipped
# under root, which ignores file-mode read permission entirely.
ws="$(new_ws unreadable)"
printf 'fn main() {\n    println!("cargo:rerun-if-changed=src");\n}\n' \
    > "${ws}/crates/pkg/build.rs"
track "${ws}"
if [[ "$(id -u)" == "0" ]]; then
    echo "skip: unreadable build.rs case (running as root)" >&2
else
    chmod 000 "${ws}/crates/pkg/build.rs"
    expect 1 "unreadable build.rs" "${ws}" 'could not analyze'
    chmod 644 "${ws}/crates/pkg/build.rs"
fi

# Case 9: perl missing from PATH must FAIL before any build.rs is scanned
# (#8306-critic) — the old gate's extractor call silently produced zero
# findings when `perl: command not found` gave it empty stdin-turned-stdout.
ws="$(new_ws noperl)"
printf 'fn main() {\n    println!("cargo:rerun-if-changed=missing.txt");\n}\n' \
    > "${ws}/crates/pkg/build.rs"
track "${ws}"
farm="${WORK}/noperl-bin"
farm_path "${farm}"
expect_path 1 "perl missing from PATH" "${ws}" "${farm}" 'perl not found'

# Live tree.
expect 0 "live repository" "${REPO_ROOT}"

if [[ "${failures}" -gt 0 ]]; then
    echo "check_buildrs_watched_paths selftest: ${failures} case(s) failed." >&2
    exit 1
fi
echo "check_buildrs_watched_paths selftest: all 9 cases and the live scan pass."
