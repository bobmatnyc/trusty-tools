# shellcheck shell=bash
#
# semver_profile.sh — per-crate SemVer surface profiles for check_semver.sh
# (issue #9278).
#
# Why: the gate's default run builds a crate with every declared feature, so it
#   never compares a crate's no-features build. trusty-memory's 1.x contract
#   freezes exactly that build: `trusty_memory::engine::*` under
#   `--no-default-features` (ADR-0066 D1.4, D4). A profile names that surface,
#   how its result is treated, and which release it is compared against.
#
# What: the table is scripts/semver-surface-profiles.tsv, six TAB-separated
#   columns, `#` comments and blank lines skipped:
#     crate     package name
#     profile   [a-z0-9][a-z0-9-]*
#     features  `-` for none (--only-explicit-features with nothing named, the
#               --no-default-features build), else a comma-separated list
#     mode      informational | blocking
#     baseline  auto (greatest stable release below the declared version) or a
#               pinned stable X.Y.Z
#     reason    free text, non-blank
#   The WHOLE table is validated before any row is used; a bad row anywhere
#   stops the run (exit 3). Informational means a COMPUTED break does not
#   block. Every fault — missing table or row, no baseline, a tool error —
#   still exits 3 (owner ruling 2026-09-26 for CHECK 5; Architect D4,
#   2026-10-13).
#
#   The parser functions are pure and sourceable alone. semver_profile_check_crate
#   uses check_semver.sh's own helpers and globals; only that script calls it.
#
# Test: `scripts/check_semver_profile_selftest.sh`.

# semver_profile_py <mode> <file> [crate profile] — the one parser.
#   validate: exit 0, or exit 1 naming `line N: <why>` on stderr.
#   row:      print `features<TAB>mode<TAB>baseline<TAB>reason`, or exit 1
#             when the table has no such row (it validates first; exit 2 on a
#             bad table).
semver_profile_py() {
  python3 - "$@" << 'PY'
import re, sys

mode, path = sys.argv[1], sys.argv[2]
NAME = re.compile(r"^[A-Za-z0-9_-]+$")
PROFILE = re.compile(r"^[a-z0-9][a-z0-9-]*$")
FEATURES = re.compile(r"^[A-Za-z0-9_/-]+(,[A-Za-z0-9_/-]+)*$")
VERSION = re.compile(r"^[0-9]+\.[0-9]+\.[0-9]+$")

def bad(n, why):
    sys.stderr.write("line %d: %s\n" % (n, why))
    raise SystemExit(1 if mode == "validate" else 2)

try:
    with open(path, encoding="utf-8") as fh:
        lines = fh.read().split("\n")
except (OSError, UnicodeDecodeError) as e:
    sys.stderr.write("cannot read %s: %s\n" % (path, e))
    raise SystemExit(1 if mode == "validate" else 2)

rows = {}
for n, line in enumerate(lines, 1):
    if not line.strip() or line.lstrip().startswith("#"):
        continue
    f = [c.strip() for c in line.split("\t")]
    if len(f) != 6:
        bad(n, "expected 6 TAB-separated fields, found %d" % len(f))
    crate, prof, feats, md, base, reason = f
    if not NAME.match(crate):
        bad(n, "crate %r is not a package name" % crate)
    if not PROFILE.match(prof):
        bad(n, "profile %r is not [a-z0-9][a-z0-9-]*" % prof)
    if feats != "-" and not FEATURES.match(feats):
        bad(n, "features %r is neither '-' nor a comma-separated list" % feats)
    if md not in ("informational", "blocking"):
        bad(n, "mode %r is neither informational nor blocking" % md)
    if base != "auto" and not VERSION.match(base):
        bad(n, "baseline %r is neither 'auto' nor a stable X.Y.Z" % base)
    if not reason:
        bad(n, "reason is blank")
    if (crate, prof) in rows:
        bad(n, "duplicate row for %s/%s" % (crate, prof))
    rows[(crate, prof)] = (feats, md, base, reason)

if mode == "validate":
    raise SystemExit(0)
hit = rows.get((sys.argv[3], sys.argv[4]))
if hit is None:
    raise SystemExit(1)
print("\t".join(hit))
PY
}

# semver_profile_validate <file> — 0 when the whole table parses.
semver_profile_validate() {
  semver_profile_py validate "$1"
}

# semver_profile_row <file> <crate> <profile> — the row's last four fields.
semver_profile_row() {
  semver_profile_py row "$1" "$2" "$3"
}

# semver_profile_feature_args <features> — one `--features` / `<name>` pair
# per line, nothing for `-`.
semver_profile_feature_args() {
  [[ "$1" == "-" ]] && return 0
  local f
  for f in ${1//,/ }; do
    printf -- '--features\n%s\n' "$f"
  done
}

# semver_profile_fail <crate> <headline> <detail...> — print a fault and exit 3.
semver_profile_fail() {
  local crate="$1" head="$2"
  shift 2
  {
    echo "FAIL: ${head} — ${crate}, profile '${PROFILE}' (${PROFILES_REL})."
    printf '      %s\n' "$@"
    echo "      Nothing was compared. This is NOT a pass (issue #9278)."
  } >&2
  exit "$EXIT_NO_VERDICT"
}

# semver_profile_check_crate <crate> — run one crate's profile and update the
# gate's tallies (checked, inventoried, noverdict, fail, PROFILE_INFO_BREAKS).
#
# Why: kept apart from the default loop so a run without --profile is the
#   command the gate issued before #9278.
# What: resolves the row and the baseline (auto: the registry's greatest
#   stable release below the declared version; pinned: that version, which must
#   be published, not yanked and below the declared version), then runs
#   cargo-semver-checks with --only-explicit-features plus the row's features.
#   An already-breaking delta adds --release-type minor, as the INVENTORY arm
#   does. A NO VERDICT counts as one whatever the mode. Prints a human result
#   line and one `semver-gate-profile:` line for a workflow to parse.
# Test: `scripts/check_semver_profile_selftest.sh` run and closed cases.
semver_profile_check_crate() {
  local crate="$1" row feats mode pin current probe latest auto_base pre_below
  local pin_seen base source rtype rc run_log blocking result label feat_label a
  label="PROFILE ${crate}/${PROFILE}"

  row="$(semver_profile_row "$PROFILES_FILE" "$crate" "$PROFILE")" ||
    semver_profile_fail "$crate" "NO PROFILE ENTRY" "The table has no row for this crate and profile."
  IFS=$'\t' read -r feats mode pin _ <<< "$row"

  if [[ "$(pkg_field "$crate" publishable)" == "no" || "$(pkg_field "$crate" has_lib)" == "no" ]]; then
    semver_profile_fail "$crate" "PROFILE TARGETS NO PUBLISHED LIBRARY" \
      "The crate is publish = false or has no library target, so it has no surface to compare."
  fi
  current="$(pkg_field "$crate" version)" || exit "$EXIT_NO_VERDICT"

  # Subshell re-raise, as everywhere registry_probe is called.
  if ! probe="$(registry_probe "$crate" "$current" "$pin")"; then
    exit "$EXIT_NO_VERDICT"
  fi
  latest="$(printf '%s\n' "$probe" | sed -n 1p)"
  auto_base="$(printf '%s\n' "$probe" | sed -n 2p)"
  pre_below="$(printf '%s\n' "$probe" | sed -n 3p)"
  pin_seen="$(printf '%s\n' "$probe" | sed -n 4p)"

  if [[ "$pin" == "auto" ]]; then
    if [[ "$latest" == "NONE" || "$auto_base" == "NONE" ]]; then
      semver_profile_fail "$crate" "NO BASELINE FOR PROFILE" \
        "No non-yanked stable release below v${current} (latest on crates.io: ${latest}; pre-release below: ${pre_below})."
    fi
    base="$auto_base"
    source="auto"
  else
    if [[ "$pin_seen" != "yes" ]]; then
      semver_profile_fail "$crate" "PINNED BASELINE NOT AVAILABLE" \
        "v${pin} is not a non-yanked release on crates.io."
    fi
    if [[ "$(release_type "$pin" "$current")" == "none" ]]; then
      semver_profile_fail "$crate" "PINNED BASELINE NOT BELOW" \
        "v${pin} is not below the declared v${current}; a release is never its own baseline (#5296)."
    fi
    base="$pin"
    source="pinned"
  fi
  rtype="$(release_type "$base" "$current")" || exit "$EXIT_NO_VERDICT"

  if [[ -z "${TOOL_OK:-}" ]]; then
    require_tool || exit "$EXIT_NO_VERDICT"
    TOOL_OK=1
  fi

  if [[ "$feats" == "-" ]]; then
    feat_label="no features — the --no-default-features build"
  else
    feat_label="features ${feats} only"
  fi
  echo "${label}: surface = ${feat_label}; mode = ${mode}"
  if [[ "$source" == "auto" ]]; then
    echo "${label}: compared v${current} against v${base} (auto: greatest stable release below v${current}), ${rtype} release"
  else
    echo "${label}: compared v${current} against v${base} (pinned in ${PROFILES_REL}), ${rtype} release"
  fi

  local -a args
  args=(--package "$crate" --baseline-version "$base")
  [[ "$rtype" == "major" ]] && args+=(--release-type minor)
  args+=(--only-explicit-features)
  while IFS= read -r a; do
    [[ -n "$a" ]] && args+=("$a")
  done <<< "$(semver_profile_feature_args "$feats")"

  rc=0
  run_log="${SCRATCH}/profile-${crate}.txt"
  semver_checks_run "${args[@]}" > "$run_log" 2>&1 || rc=$?
  cat "$run_log"

  blocking="no"
  if ! verdict_computed "$run_log"; then
    echo "NO VERDICT ${crate} (profile ${PROFILE}): cargo semver-checks exited ${rc} and $(no_verdict_because)." >&2
    echo "           Informational covers a computed break only; a run that compared nothing is a fault." >&2
    scratch_resolution_report "$crate" "$run_log"
    noverdict=$((noverdict + 1))
    result="no-verdict"
  elif [[ "$rc" -ne 0 ]]; then
    result="break"
    if [[ "$mode" == "blocking" && "$rtype" != "major" ]]; then
      blocking="yes"
      echo "FAIL ${crate}: profile ${PROFILE} (mode blocking) found breaking change(s) against v${base}" >&2
      # shellcheck disable=SC2034 # read by check_semver.sh after the loop
      fail=1
    else
      local why="mode informational (Architect D4, 2026-10-13: pin the baseline, then set blocking)"
      [[ "$rtype" == "major" ]] && why="v${base} -> v${current} is already a breaking release"
      echo "${label}: INFORMATIONAL — breaking change(s) listed above against v${base}. Not blocking: ${why}."
      PROFILE_INFO_BREAKS=$((PROFILE_INFO_BREAKS + 1))
    fi
  else
    result="clean"
    echo "${label}: no breaking changes found against v${base}."
  fi
  if [[ "$result" != "no-verdict" ]]; then
    if [[ "$rtype" == "major" ]]; then
      inventoried=$((inventoried + 1))
    else
      checked=$((checked + 1))
    fi
  fi

  printf 'semver-gate-profile: crate=%s profile=%s mode=%s features=%s baseline=%s baseline_source=%s release=%s result=%s blocking=%s\n' \
    "$crate" "$PROFILE" "$mode" "$([[ "$feats" == "-" ]] && echo none || echo "$feats")" \
    "$base" "$source" "$rtype" "$result" "$blocking"
}
