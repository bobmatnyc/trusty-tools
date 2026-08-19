# scripts/lib/build_accel.sh — shared build-acceleration resolver for the
# publish preflight path.
#
# Why: the preflight's one compiling step is `cargo semver-checks`, run by
#   scripts/check_semver.sh. It builds rustdoc for the crate under test and for
#   its whole dependency closure, in a scratch project under the workspace's
#   target directory. A fresh worktree has no such directory, so every worktree
#   compiles openssl-sys and the rest of the closure from zero — and this repo's
#   worktree discipline means fresh worktrees are the normal case, not the
#   exception.
#
#   sccache fixes exactly that: it caches compiler OUTPUTS keyed by input hash,
#   so a worktree cargo has never seen still gets its dependency closure from
#   cache instead of recompiling it. Measured on trusty-embedderd, a cold target
#   directory went from 49.8s to 18.9s with a warm sccache, at 347 of 347
#   compile requests served from cache.
#
#   OFF BY CONSTRUCTION WHEN UNAVAILABLE. No sccache on PATH resolves to empty,
#   and empty means the caller runs exactly the command it ran before this file
#   existed — no error, no warning, one log line saying which mode it is in.
#
# What: two functions. They read the environment and PATH and PRINT; neither
#   exports a variable into the caller's environment, and neither compiles
#   anything. The caller applies what they print as a per-command `env` prefix,
#   which is what keeps the wrapper off every other process the preflight spawns.
#
#     build_accel_sccache             -> absolute path to sccache, or nothing
#     build_accel_mode_line <sccache> -> one line naming the mode
#
# A WRAPPER CANNOT MAKE A GATE PASS. RUSTC_WRAPPER changes which process invokes
#   rustc; it does not change what rustc is invoked on, what the crate's public
#   API is, or what cargo-semver-checks compares. A cache that served a wrong
#   object would fail the build, and a failed build prints no `N checks:` summary
#   line, which check_semver.sh's verdict_computed classifies as NO VERDICT
#   (exit 3) — the same loud stop a rustdoc crash gets, never a skip and never a
#   pass. scripts/check_semver_selftest.sh case 27 pins that.
#
# WHY THERE IS NO PERSISTENT CARGO_TARGET_DIR HERE. A persistent target directory
#   for the SemVer gate was designed, built and then REJECTED on evidence; this
#   note exists so the next person does not rediscover the same trap.
#   cargo-semver-checks 0.50.0 has no --target-dir flag, so the only mechanism is
#   the CARGO_TARGET_DIR environment variable — and it is honoured by the tool's
#   INNER `cargo doc` invocation as well as its own bookkeeping. Setting it moves
#   the current crate's rustdoc JSON out of
#       <target>/semver-checks/local-<pkg>-<ver>-<triple>-<hash>/target/doc/<pkg>.json
#   and into a flat
#       <target>/doc/<pkg>.json
#   keyed by crate NAME alone — no version, no feature-set hash. Two
#   consequences, both measured, neither acceptable:
#     1. scripts/check_semver_types.sh globs the versioned path, finds nothing,
#        and reports NO VERDICT. Preflight CHECK 5b is advisory, so it does not
#        block a publish — it just silently stops covering the type substitutions
#        cargo-semver-checks is known to miss.
#     2. Worse, the flat path is SHARED. The whole point of a persistent
#        directory is that worktrees share it, and this repo runs gates in
#        parallel worktrees. Two concurrent runs on the same crate write the same
#        file, so the differ can read another worktree's source and report a
#        result about the wrong tree. That is a stale cache changing an answer,
#        which is the one thing this file is not allowed to introduce.
#   The differ's per-version, per-feature-hash directory and its
#   refuse-on-ambiguity guard are what prevent exactly that mixing, and
#   CARGO_TARGET_DIR flattens both away. sccache gets the cross-worktree win
#   without touching where any artifact lands.
#
# Portability: bash 3.2 (macOS system bash) and bash 5 (Linux CI).
#
# Test: `scripts/build_accel_selftest.sh` covers detection, the opt-out, and the
#   mode line. `scripts/check_semver_selftest.sh` cases 25-27 cover the wiring:
#   what reaches the cargo subprocess, that the opt-out reaches it, that
#   CARGO_TARGET_DIR is NOT injected, and that a build failure under the wrapper
#   still exits 3.

# ---------------------------------------------------------------------------
# build_accel_sccache — print the absolute path to sccache, or nothing.
#
# Auto-detect, with one explicit opt-out. PREFLIGHT_NO_SCCACHE disables the
# wrapper when it is set to anything other than empty or `0`; `0` is spelled out
# as "not an opt-out" because a variable named NO_<thing> set to zero reads as
# "do not disable" to everyone who writes it, and silently meaning the opposite
# is the kind of trap that only surfaces during a release.
#
# Absence is not an error and not a warning. A machine without sccache runs the
# preflight exactly as it did before; the mode line says which mode it is in, and
# that one line is the whole report.
# ---------------------------------------------------------------------------
build_accel_sccache() {
  case "${PREFLIGHT_NO_SCCACHE:-}" in
    "" | 0) ;;
    *) return 0 ;;
  esac
  command -v sccache 2> /dev/null || true
}

# ---------------------------------------------------------------------------
# build_accel_mode_line <sccache-path> — the one line a caller prints to say
# which mode it is in. The argument may be empty.
#
# One line, always, in every mode. The absent case is the common one on a CI
# runner and must not read as a problem to fix. An opt-out and an absent binary
# are reported as different facts: calling the opt-out "not installed" would send
# an operator to `brew install` over a variable they set themselves.
# ---------------------------------------------------------------------------
build_accel_mode_line() {
  local sccache="${1:-}"

  if [ -n "$sccache" ]; then
    echo "build-accel: sccache ${sccache} (RUSTC_WRAPPER)"
    return 0
  fi

  case "${PREFLIGHT_NO_SCCACHE:-}" in
    "" | 0) echo "build-accel: no sccache on PATH; rustc runs unwrapped" ;;
    *) echo "build-accel: sccache disabled by PREFLIGHT_NO_SCCACHE; rustc runs unwrapped" ;;
  esac
}
