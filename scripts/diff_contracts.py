"""Diff two Code Contract artifacts — the behavioural half of the API gate.

Why: this is the payoff of extracting contracts into an artifact at all. A
  contract change on an item present in BOTH versions is a behavioural break
  under a signature that may not have moved at all, which is the one thing
  neither `cargo-semver-checks` nor `check_semver_types.sh` can ever see. The
  instance on record is `latest_trusty_mpm_snapshot`: byte-identical signature
  across trusty-common 0.24.2..0.34.0, inverted precondition (#5272), every tool
  clean. Run this between a published baseline and a release candidate and that
  break becomes a listed finding instead of a shipped surprise. See ADR-0047.

What: reports, per contracted item present in both artifacts, every claim added
  to or removed from `preconditions`, `postconditions`, and `invariants`.
  Claims are compared as SETS of exact strings — a reworded claim reports as one
  removal plus one addition, which is the correct conservative reading: a
  machine cannot tell a clarification from a change of meaning, and asking a
  human to look is the safe direction.

  Items present in only one artifact are COUNTED, never failed on. An added
  contract is the incremental adoption ADR-0047 describes; a removed one is
  reported as a count so it cannot pass unnoticed, but a deleted item is
  cargo-semver-checks' finding to make and double-reporting it here would only
  add noise.

Exit:
  0  compared at least one item present in both; no contract changed
  1  contract changes found, listed on stdout
  3  NO VERDICT — nothing was compared, reason on stderr

Test: `scripts/check_contracts_selftest.sh`.
"""

import json
import sys

# The artifact schema this differ understands. An artifact written by a newer
# extractor is a NO VERDICT rather than a best-effort read: a field this does
# not know about could be the one carrying the change.
SUPPORTED_ARTIFACT_VERSIONS = (1,)

SECTIONS = ("preconditions", "postconditions", "invariants")

NO_VERDICT = 3


class Unreadable(Exception):
    pass


def load(path, label):
    try:
        with open(path) as fh:
            doc = json.load(fh)
    except OSError as e:
        raise Unreadable("%s artifact %s could not be read: %s" % (label, path, e))
    except ValueError as e:
        raise Unreadable("%s artifact %s did not parse: %s" % (label, path, e))
    if not isinstance(doc, dict) or "items" not in doc:
        raise Unreadable("%s file %s is JSON but not a contract artifact" % (label, path))
    av = doc.get("artifact_version")
    if av not in SUPPORTED_ARTIFACT_VERSIONS:
        raise Unreadable(
            "%s artifact %s has artifact_version %r; this differ understands %s"
            % (label, path, av, list(SUPPORTED_ARTIFACT_VERSIONS))
        )
    by_key = {}
    for it in doc["items"]:
        key = "%s %s" % (it.get("kind"), it.get("path"))
        if key in by_key:
            raise Unreadable("%s artifact %s lists %s twice" % (label, path, key))
        by_key[key] = it
    return by_key


def main(argv):
    if len(argv) != 3:
        print("usage: diff_contracts.py <baseline.json> <current.json>", file=sys.stderr)
        return NO_VERDICT
    try:
        base = load(argv[1], "baseline")
        cur = load(argv[2], "current")
    except Unreadable as e:
        print("NO VERDICT: %s" % e, file=sys.stderr)
        return NO_VERDICT

    common = sorted(set(base) & set(cur))
    if not common:
        print(
            "NO VERDICT: 0 contracted items are present in both artifacts, so "
            "nothing was compared. A differ that compared nothing has not "
            "agreed with anything (#5620).",
            file=sys.stderr,
        )
        return NO_VERDICT

    changed = 0
    for key in common:
        for section in SECTIONS:
            was = set(base[key].get(section) or [])
            now = set(cur[key].get(section) or [])
            for claim in sorted(was - now):
                print("REMOVED %s [%s]: %s" % (key, section, claim))
                changed += 1
            for claim in sorted(now - was):
                print("ADDED   %s [%s]: %s" % (key, section, claim))
                changed += 1

    print(
        "compared: %d contracted item(s) in both; %d claim change(s), "
        "%d contract(s) only in baseline, %d only in current"
        % (len(common), changed, len(set(base) - set(cur)), len(set(cur) - set(base)))
    )
    if changed:
        print(
            "\nCONTRACT CHANGE(S) FOUND, listed above. Each one is a change to what a "
            "caller may rely on, on an item that exists in both versions — so none of "
            "them will appear in cargo-semver-checks or in the type differ, however "
            "strict those are set.\n"
            "Confirm every one was intended, and that the version bump matches.",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
