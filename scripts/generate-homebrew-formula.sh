#!/usr/bin/env bash
#
# generate-homebrew-formula.sh — render tap/Formula/<crate>.rb for the
# bobmatnyc/homebrew-trusty tap.
#
# Why (#5635): this logic used to live as an inline Python heredoc inside a `run:`
#   step inside `.github/workflows/release.yml`. Three layers of quoting, and no
#   way to run it, test it, or diff its output without pushing a tag. The only
#   observation anyone ever got of it was the formula that appeared in the tap
#   afterwards. It also carried its OWN copy of the crate→binary map, a second
#   spelling of `CRATE_CONFIG`'s `BINARIES` in the same file — two maps that
#   agreed today only because nobody had added a crate since.
#
#   It is one implementation now, and it is this file. `release.yml` calls it
#   and passes `needs.setup.outputs.binaries`, so the binary map has one
#   spelling too.
#
# What: writes `<formula-dir>/<crate>.rb` from the crate name, version, tag, and
#   the SHA-256 digests of the two Tier-1 release assets (aarch64-apple-darwin,
#   x86_64-unknown-linux-gnu). The digests are READ from the `.tar.gz.sha256`
#   sidecars the build job already uploaded — this script never computes one, so
#   it cannot disagree with what was released.
#
#   Every check runs; none short-circuits. Findings on stderr as [PASS]/[FAIL]/
#   [WARN], one summary line on stdout:
#
#     N crate(s) considered, M formula(e) updated
#
#   THE SUMMARY IS THE POINT. A run that renders a formula byte-identical to the
#   one already on disk updated NOTHING, and saying so at exit 0 would report
#   success from the evidence that work happened rather than from a count of
#   work done. That run exits 3 — NO VERDICT — the same convention
#   check_semver.sh uses for "ran but verified nothing" (#5289). A caller that
#   wants a no-op has to ask for one (--expect-unchanged); it never gets one by
#   accident.
#
# Usage:
#   scripts/generate-homebrew-formula.sh \
#     --crate <name> --version <X.Y.Z> --tag <tag> --binaries "<b1 b2 ...>" \
#     [--assets-dir <dir>] [--formula-dir <dir>] [--repo-slug <owner/repo>] \
#     [--macos-sha256 <hex>] [--linux-sha256 <hex>] [--expect-unchanged]
#
#     --crate      crate name as released, e.g. trusty-review. `tga-v*` tags are
#                  already canonicalised to trusty-git-analytics upstream
#                  (#1128), so this is always the canonical name.
#     --version    the crate version, X.Y.Z. Appears in the formula and in every
#                  asset filename.
#     --tag        the release tag the assets hang off, e.g. tga-v2.19.0. NOT
#                  derivable from crate+version — the tga alias series is why
#                  (#5455).
#     --binaries   space-separated binaries to `bin.install`, in order. The first
#                  is the `test do` smoke-test target. In CI this is
#                  `needs.setup.outputs.binaries`, straight from CRATE_CONFIG.
#     --assets-dir directory searched for <crate>-<version>-<target>.tar.gz.sha256
#                  (default: release-assets). Ignored when both --*-sha256 are
#                  given.
#     --formula-dir where <crate>.rb is written (default: tap/Formula).
#     --repo-slug   owner/repo the release assets hang off
#                   (default: bobmatnyc/trusty-tools).
#     -h|--help     print this header.
#
# Escape hatches (env; each announces itself as a loud [WARN], never silent):
#
#   HOMEBREW_FORMULA_EXPECT_UNCHANGED=1   same as --expect-unchanged. INVERTS
#       the verdict: the caller asserts this run should change nothing, so a
#       no-op becomes exit 0 and an actual update becomes exit 1. This is the
#       mode that proves byte-identity against the live tap, and the mode the
#       self-test drives. It does not weaken the default contract — it states a
#       different expectation, and still fails when that expectation is wrong.
#
#   HOMEBREW_FORMULA_SKIP_SHA_FORMAT=1    accept a digest that is not 64 lowercase
#       hex characters. Only reachable if the build job's sha256 sidecar format
#       ever changes; leaving it on ships a formula Homebrew will reject at
#       install time.
#
# Exit codes (1 and 3 are different facts, per check_semver.sh's convention):
#   0  a verdict, and it matches the expectation: M >= 1 normally, M == 0 under
#      --expect-unchanged.
#   1  a check FAILED, or the verdict contradicts --expect-unchanged.
#   2  usage error.
#   3  NO VERDICT — the run considered at least one crate and updated zero
#      formulae without being asked to. Nothing was written, so nothing may be
#      concluded about the tap. In release.yml this is the idempotent re-run:
#      the formula was already at this version, so there is nothing to commit.
#
# Test: scripts/generate-homebrew-formula-selftest.sh, which renders every
#   formula the live tap holds from its recorded inputs and asserts the bytes
#   match, then pins the exit contract — including exit 3 for a no-op run, which
#   is the status a passing gate must never be able to reach by accident. Run it
#   directly: bash scripts/generate-homebrew-formula-selftest.sh
#
# Portability: POSIX tools only, bash 3.2 (macOS) and bash 5 (Linux CI). No GNU
#   sed — the class-name conversion was `s/-\([a-z]\)/\U\1/g`, which is a GNU
#   extension that silently does nothing on BSD sed, so it is open-coded below.

set -euo pipefail

for arg in "$@"; do
  case "$arg" in
    -h|--help)
      grep '^#' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
  esac
done

SELF="generate-homebrew-formula"

usage() {
  echo "usage: scripts/generate-homebrew-formula.sh --crate <name> --version <X.Y.Z>" >&2
  echo "                                            --tag <tag> --binaries \"<b1 b2 ...>\"" >&2
  echo "                                            [--assets-dir <dir>] [--formula-dir <dir>]" >&2
  echo "                                            [--repo-slug <owner/repo>]" >&2
  echo "                                            [--macos-sha256 <hex>] [--linux-sha256 <hex>]" >&2
  echo "                                            [--expect-unchanged]" >&2
  exit 2
}

CRATE=""
VERSION=""
TAG=""
BINARIES=""
ASSETS_DIR="release-assets"
FORMULA_DIR="tap/Formula"
REPO_SLUG="bobmatnyc/trusty-tools"
MACOS_SHA256=""
LINUX_SHA256=""
EXPECT_UNCHANGED="${HOMEBREW_FORMULA_EXPECT_UNCHANGED:-0}"
SKIP_SHA_FORMAT="${HOMEBREW_FORMULA_SKIP_SHA_FORMAT:-0}"

need_value() { [ "$1" -ge 2 ] || usage; }

while [ "$#" -gt 0 ]; do
  case "$1" in
    --crate)         need_value "$#"; CRATE="$2";        shift 2 ;;
    --version)       need_value "$#"; VERSION="$2";      shift 2 ;;
    --tag)           need_value "$#"; TAG="$2";          shift 2 ;;
    --binaries)      need_value "$#"; BINARIES="$2";     shift 2 ;;
    --assets-dir)    need_value "$#"; ASSETS_DIR="$2";   shift 2 ;;
    --formula-dir)   need_value "$#"; FORMULA_DIR="$2";  shift 2 ;;
    --repo-slug)     need_value "$#"; REPO_SLUG="$2";    shift 2 ;;
    --macos-sha256)  need_value "$#"; MACOS_SHA256="$2"; shift 2 ;;
    --linux-sha256)  need_value "$#"; LINUX_SHA256="$2"; shift 2 ;;
    --expect-unchanged) EXPECT_UNCHANGED=1; shift ;;
    *)
      echo "${SELF}: unknown argument: $1" >&2
      usage
      ;;
  esac
done

[ -n "$CRATE" ] && [ -n "$VERSION" ] && [ -n "$TAG" ] && [ -n "$BINARIES" ] || usage

# No repo-root resolution, unlike its siblings in scripts/: this script reads
# nothing from the checkout. --assets-dir and --formula-dir resolve against the
# CWD, which is what the CI job wants (the tap is checked out beside the
# artifacts, not inside this repo) and what a hand-run wants too.

SCRATCH="$(mktemp "${TMPDIR:-/tmp}/generate-homebrew-formula.render.XXXXXX")"
trap 'rm -f "$SCRATCH"' EXIT

FAILURES=0
fail() { echo "[FAIL] $*" >&2; FAILURES=$((FAILURES + 1)); }
pass() { echo "[PASS] $*" >&2; }
warn() { echo "[WARN] $*" >&2; }

if [ "$EXPECT_UNCHANGED" = "1" ]; then
  warn "HOMEBREW_FORMULA_EXPECT_UNCHANGED / --expect-unchanged is ON: this run asserts"
  warn "       the formula is ALREADY correct. A no-op is exit 0 and an update is exit 1 —"
  warn "       the inverse of the normal contract. Never set this in release.yml."
fi
if [ "$SKIP_SHA_FORMAT" = "1" ]; then
  warn "HOMEBREW_FORMULA_SKIP_SHA_FORMAT=1: digest format is NOT validated. A malformed"
  warn "       sha256 renders a formula that fails at 'brew install', not here."
fi

# ---------------------------------------------------------------------------
# Checks. Each is its own function and each RUNS — findings accumulate in
# FAILURES rather than aborting, so one bad input never hides the next.
# ---------------------------------------------------------------------------

# check_identity: crate/version/tag well-formed, and the tag actually names this
# version. A tag naming a different version would render URLs pointing at assets
# that do not exist, and Homebrew would only discover that on `brew install`.
check_identity() {
  case "$CRATE" in
    *[!a-zA-Z0-9-]*|"") fail "crate name '${CRATE}' is not [A-Za-z0-9-]+" ;;
    *) pass "crate name: ${CRATE}" ;;
  esac

  case "$VERSION" in
    [0-9]*.[0-9]*.[0-9]*) pass "version: ${VERSION}" ;;
    *) fail "version '${VERSION}' is not X.Y.Z" ;;
  esac

  case "$TAG" in
    *"v${VERSION}") pass "tag ${TAG} ends in v${VERSION}" ;;
    *) fail "tag '${TAG}' does not end in 'v${VERSION}' — the tag and the version disagree,
       so the asset URLs this would render point at a release that does not exist" ;;
  esac
}

# check_binaries: at least one binary, each a plausible executable name. The
# FIRST one is the smoke-test target: the formula's `test do` block runs
# `<bin> --version`, and before #896's follow-up that target was
# crate.split("-")[0], i.e. "trusty" for every trusty-* crate — a binary present
# in no tarball. Order is therefore load-bearing and is preserved verbatim.
check_binaries() {
  local n=0 b
  for b in $BINARIES; do
    n=$((n + 1))
    case "$b" in
      *[!a-zA-Z0-9._-]*) fail "binary name '${b}' contains characters no release asset carries" ;;
    esac
  done
  if [ "$n" -eq 0 ]; then
    fail "--binaries is empty; the formula would install nothing"
  else
    pass "${n} binary/binaries: ${BINARIES} (smoke-test target: $(printf '%s' "$BINARIES" | awk '{print $1}'))"
  fi
}

# resolve_sha256: prefer an explicitly passed digest; otherwise read the sidecar
# the build job uploaded. Never computes one — a digest computed here could
# describe a tarball other than the one the release actually serves.
resolve_sha256() {
  local target="$1" preset="$2" file digest
  if [ -n "$preset" ]; then
    printf '%s' "$preset"
    return 0
  fi
  file="$(find "$ASSETS_DIR" -name "${CRATE}-${VERSION}-${target}.tar.gz.sha256" 2>/dev/null | head -1)"
  [ -n "$file" ] || return 1
  # sha256sum / `shasum -a 256` format: "<hex>  <filename>".
  digest="$(awk '{print $1}' "$file" | head -1)"
  [ -n "$digest" ] || return 1
  printf '%s' "$digest"
}

# check_digests: both Tier-1 platforms must resolve to a well-formed digest.
# Homebrew has exactly one url/sha256 pair per platform block, so a missing
# digest is not degradable into a partial formula.
check_digests() {
  local target digest
  for target in aarch64-apple-darwin x86_64-unknown-linux-gnu; do
    case "$target" in
      aarch64-apple-darwin)      digest="$(resolve_sha256 "$target" "$MACOS_SHA256" || true)" ;;
      x86_64-unknown-linux-gnu)  digest="$(resolve_sha256 "$target" "$LINUX_SHA256" || true)" ;;
    esac

    if [ -z "$digest" ]; then
      fail "no SHA-256 for ${CRATE}-${VERSION}-${target}: neither passed explicitly nor
       found as ${ASSETS_DIR}/**/${CRATE}-${VERSION}-${target}.tar.gz.sha256"
      if [ -d "$ASSETS_DIR" ]; then
        echo "       sidecars present under ${ASSETS_DIR}:" >&2
        find "$ASSETS_DIR" -name '*.sha256' 2>/dev/null | sort | sed 's/^/         /' >&2
      else
        echo "       ${ASSETS_DIR} does not exist" >&2
      fi
      continue
    fi

    if [ "$SKIP_SHA_FORMAT" != "1" ]; then
      case "$digest" in
        [0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f]*)
          if [ "${#digest}" -ne 64 ]; then
            fail "${target} digest is ${#digest} characters, not 64: ${digest}"
            continue
          fi
          ;;
        *)
          fail "${target} digest is not lowercase hex: ${digest}"
          continue
          ;;
      esac
    fi

    case "$target" in
      aarch64-apple-darwin)     MACOS_SHA256="$digest" ;;
      x86_64-unknown-linux-gnu) LINUX_SHA256="$digest" ;;
    esac
    pass "${target} SHA-256: ${digest}"
  done
}

# check_formula_dir: the destination must be creatable and writable. Failing here
# rather than at the write keeps the "0 updated" summary honest — a write that
# silently could not happen is exactly what the exit-3 rule exists to catch.
check_formula_dir() {
  if ! mkdir -p "$FORMULA_DIR" 2>/dev/null; then
    fail "cannot create formula directory ${FORMULA_DIR}"
    return
  fi
  if [ ! -w "$FORMULA_DIR" ]; then
    fail "formula directory ${FORMULA_DIR} is not writable"
    return
  fi
  pass "formula directory: ${FORMULA_DIR}"
}

set +e
check_identity
check_binaries
check_digests
check_formula_dir
set -e

if [ "$FAILURES" -ne 0 ]; then
  echo "${SELF}: ${FAILURES} check(s) FAILED — no formula written." >&2
  echo "0 crate(s) considered, 0 formula(e) updated"
  exit 1
fi

# ---------------------------------------------------------------------------
# Render.
# ---------------------------------------------------------------------------

# to_class_name: "trusty-git-analytics" -> "TrustyGitAnalytics".
#
# This reproduces `sed 's/-\([a-z]\)/\U\1/g; s/^\([a-z]\)/\U\1/'` exactly,
# INCLUDING its edge: a dash followed by anything other than [a-z] is KEPT. That
# never fires for a real crate name, but reproducing it is what makes byte
# identity with the formulae already in the tap a property of this function
# rather than a coincidence of the current crate list.
#
# shellcheck disable=SC2018,SC2019  # 'a-z'/'A-Z' is deliberate, not an oversight:
#   [:lower:]/[:upper:] are locale-aware, and a runner with a Turkish locale would
#   render "Trusty" as "TRUSTY" with a dotless I. The sed this replaces matched
#   ASCII [a-z]; so does this.
to_class_name() {
  local s="$1" out="" i=0 c next
  local len=${#s}
  while [ "$i" -lt "$len" ]; do
    c="${s:$i:1}"
    if [ "$c" = "-" ] && [ $((i + 1)) -lt "$len" ]; then
      next="${s:$((i + 1)):1}"
      case "$next" in
        [a-z])
          out="${out}$(printf '%s' "$next" | tr 'a-z' 'A-Z')"
          i=$((i + 2))
          continue
          ;;
      esac
    fi
    out="${out}${c}"
    i=$((i + 1))
  done
  case "${out:0:1}" in
    [a-z]) out="$(printf '%s' "${out:0:1}" | tr 'a-z' 'A-Z')${out:1}" ;;
  esac
  printf '%s' "$out"
}

CLASS_NAME="$(to_class_name "$CRATE")"
BASE_URL="https://github.com/${REPO_SLUG}/releases/download/${TAG}"
MACOS_URL="${BASE_URL}/${CRATE}-${VERSION}-aarch64-apple-darwin.tar.gz"
LINUX_URL="${BASE_URL}/${CRATE}-${VERSION}-x86_64-unknown-linux-gnu.tar.gz"

BIN_LINES=""
for bin in $BINARIES; do
  BIN_LINES="${BIN_LINES}${BIN_LINES:+
}    bin.install \"${bin}\""
done
TEST_BIN="$(printf '%s' "$BINARIES" | awk '{print $1}')"

# The heredoc is UNQUOTED so ${VAR} interpolates; the formula text itself
# contains no `$`, no backtick and no backslash, so nothing else expands.
cat > "$SCRATCH" <<EOF
# Generated by the Binary Release workflow (release.yml).
# Do not edit by hand — changes will be overwritten on the next release.
# To customise behaviour, edit .github/workflows/release.yml in
# bobmatnyc/trusty-tools (homebrew-bump job, "Generate or update Formula" step).
class ${CLASS_NAME} < Formula
  desc "trusty-tools: ${CRATE} binary"
  homepage "https://github.com/${REPO_SLUG}"
  version "${VERSION}"

  # macOS arm64 (Apple Silicon) pre-built binary
  on_macos do
    on_arm do
      url "${MACOS_URL}"
      sha256 "${MACOS_SHA256}"
    end
  end

  # Linux x86_64 (glibc 2.17+) pre-built binary
  on_linux do
    on_intel do
      url "${LINUX_URL}"
      sha256 "${LINUX_SHA256}"
    end
  end

  def install
${BIN_LINES}
  end

  test do
    system bin/"${TEST_BIN}", "--version"
  end
end
EOF

FORMULA_FILE="${FORMULA_DIR}/${CRATE}.rb"
CONSIDERED=1
UPDATED=0

if [ -f "$FORMULA_FILE" ] && cmp -s "$SCRATCH" "$FORMULA_FILE"; then
  pass "${FORMULA_FILE} is already byte-identical — nothing to write"
else
  cp "$SCRATCH" "$FORMULA_FILE"
  UPDATED=1
  pass "wrote ${FORMULA_FILE}"
fi

SUMMARY="${CONSIDERED} crate(s) considered, ${UPDATED} formula(e) updated"

# ---------------------------------------------------------------------------
# Verdict. A count of work done, never evidence that work was attempted.
# ---------------------------------------------------------------------------
if [ "$EXPECT_UNCHANGED" = "1" ]; then
  if [ "$UPDATED" -ne 0 ]; then
    echo "[FAIL] --expect-unchanged was asserted, but ${FORMULA_FILE} CHANGED." >&2
    echo "       The formula on disk is not what these inputs render. Diff:" >&2
    diff -u "$FORMULA_FILE" "$SCRATCH" 2>/dev/null | sed 's/^/         /' >&2 || true
    echo "$SUMMARY"
    exit 1
  fi
  echo "$SUMMARY — unchanged, as asserted."
  exit 0
fi

if [ "$UPDATED" -eq 0 ]; then
  cat >&2 <<'EOF'
[WARN] NO VERDICT: this run wrote no formula.

  The rendered formula is byte-identical to the one already on disk, so nothing
  about the tap was changed and nothing about it can be concluded from this run.
  That is exit 3, not exit 0 — "the formula did not need writing" and "the
  formula was written" are different facts and must not share a status.

  In release.yml this is the idempotent re-run of a tag that already shipped:
  there is nothing to commit, and the job treats 3 as success. Anywhere else it
  means the inputs describe a release the tap already has. If you MEANT to
  assert that, say so with --expect-unchanged and get a verdict instead.
EOF
  echo "$SUMMARY"
  exit 3
fi

echo "$SUMMARY"
exit 0
