"""Extract Code Contracts from a crate's rustdoc JSON into a versioned artifact.

Why: `cargo-semver-checks` compares existence and shape; `check_semver_types.sh`
  (#5723) added types. Neither can see a BEHAVIOURAL change under an unchanged
  signature, and no static differ ever will. The instance on record is
  `catchup::session_finder::latest_trusty_mpm_snapshot`, whose signature is
  byte-identical from trusty-common 0.24.2 through 0.34.0 while its
  PRECONDITION inverted: passing `None` for the session id used to mean "give me
  the newest snapshot overall" and now means "nothing is attributable to me,
  return `None`" (#5272). Every tool reported clean and the break shipped.

  A contract is checkable only if it lives somewhere a machine can read. Stating
  it in prose inside a doc comment is discipline; extracting it into an artifact
  makes it evidence. Once the contract is in the artifact, a contract change is
  mechanically detectable across two published versions — exactly the way the
  type differ catches `u64` -> `Result<u64>`.

What: walks the public surface via the shared `scripts/lib/rustdoc_walk.py`
  (the same walk the type differ uses), parses the `# Code Contract` block out of
  each item's doc comment, and writes a stable, sorted JSON artifact.

FAIL CLOSED. This repo has been bitten twice by a gate that reported success
  while checking nothing (#5620's `0 compared` printing `[PASS]`, and the type
  gap #5723 closed). Every one of these is a hard error, never an empty result:
    - the document is missing, unreadable, not rustdoc, or an unverified schema
    - a `# Code Contract` heading whose body does not parse under the grammar below
    - a contract with no claims in any section
    - ZERO contracts found in a crate that was asked for them
  An item WITHOUT a `# Code Contract` block is not an error — contracts are being
  adopted incrementally (ADR-0047). What is an error is a block that exists and
  cannot be read, because that is indistinguishable from a contract that was
  silently dropped.

# The grammar

Inside a doc comment, after the repo's mandatory `Why:` / `What:` / `Test:`
lines, a contracted item carries ONE block:

    /// # Code Contract
    /// Preconditions:
    /// - <one claim, on one line>
    /// - <another claim>
    /// Postconditions:
    /// - <one claim>
    /// Invariants:
    /// - <one claim>

Rules, all of them mechanical — a parser reads this without heuristics:
  * The block opens on a line that is exactly `# Code Contract` and closes at the
    next line starting with `# `, or at the end of the doc comment.
  * Section headers are exactly `Preconditions:`, `Postconditions:`, or
    `Invariants:` on their own line. Any section may be omitted; at least one
    must be present and carry at least one claim.
  * A claim is a line starting with `- `. One claim per line. A line indented
    under a claim continues it and is joined with a single space, so a long
    claim wraps without becoming two claims.
  * Blank lines are ignored. ANY OTHER LINE inside the block is a parse error.
    Free prose inside the block is refused rather than skipped, because a
    silently-skipped line is how a claim goes missing.

Usage:
  python3 scripts/extract_contracts.py <rustdoc.json> <crate-name> [--out <path>]
  (with no --out, the artifact is written to stdout)

Exit:
  0  wrote an artifact containing at least one contract
  3  NO VERDICT — nothing was extracted, reason on stderr. Never a silent pass.

Test: `scripts/check_contracts_selftest.sh`.
"""

import json
import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "lib"))

from rustdoc_walk import SurfaceWalker, Unrecognised, load  # noqa: E402

# The schema of the ARTIFACT this script writes — not of the rustdoc input.
# Bump when the artifact's shape changes; the consumer refuses an unknown value.
ARTIFACT_VERSION = 1

# Rustdoc JSON schemas this extractor has been checked against. Deliberately
# separate from check_semver_types.sh's tuple: that gate reads documents built
# by cargo-semver-checks' pinned nightly, this one reads documents built by the
# repo's own nightly, and they move independently.
SUPPORTED_FORMAT_VERSIONS = (57, 61)

HEADING = "# Code Contract"
SECTIONS = {
    "Preconditions:": "preconditions",
    "Postconditions:": "postconditions",
    "Invariants:": "invariants",
}

NO_VERDICT = 3


class ContractParseError(Exception):
    """A `# Code Contract` block that exists but cannot be read under the grammar.

    Always fatal. A block that parsed to nothing is indistinguishable from a
    contract that was silently dropped, which is the failure this whole
    mechanism exists to make impossible.
    """


def parse_contract(docs, where):
    """Parse the `# Code Contract` block out of one doc comment.

    Returns None when the item carries no block at all — the normal state for an
    uncontracted item. Raises ContractParseError when a block is present and
    malformed.
    """
    if not docs:
        return None
    lines = docs.split("\n")
    start = None
    for i, raw in enumerate(lines):
        if raw.strip() == HEADING:
            start = i + 1
            break
    if start is None:
        return None

    body = []
    for raw in lines[start:]:
        if raw.startswith("# "):
            break
        body.append(raw)

    out = {"preconditions": [], "postconditions": [], "invariants": []}
    current = None
    for raw in body:
        line = raw.strip()
        if not line:
            continue
        if line in SECTIONS:
            current = SECTIONS[line]
            continue
        if line.startswith("- "):
            if current is None:
                raise ContractParseError(
                    "%s: claim %r appears before any of %s"
                    % (where, line, ", ".join(sorted(SECTIONS)))
                )
            out[current].append(line[2:].strip())
            continue
        # A continuation of the claim above: indented in the source, and only
        # legal when a claim is already open.
        if raw.startswith((" ", "\t")) and current is not None and out[current]:
            out[current][-1] = "%s %s" % (out[current][-1], line)
            continue
        raise ContractParseError(
            "%s: line %r is inside the # Code Contract block but is neither a section "
            "header, a '- ' claim, nor a continuation of one. Free prose is "
            "refused here — a skipped line is how a claim goes missing." % (where, line)
        )

    if not any(out.values()):
        raise ContractParseError(
            "%s: the # Code Contract block declares no claims. An empty contract "
            "promises nothing and would compare equal to a deleted one." % where
        )
    return out


def extract(doc, crate):
    """Walk the public surface and collect every contracted item."""
    items = []
    for kind, qual, it in SurfaceWalker(doc).walk():
        parsed = parse_contract(it.get("docs"), "%s %s" % (kind, qual))
        if parsed is None:
            continue
        entry = {"kind": kind, "path": qual}
        entry.update(parsed)
        items.append(entry)
    # Sorted so the artifact is a stable function of the source, not of the
    # traversal order or of rustdoc's per-build item ids.
    items.sort(key=lambda e: (e["path"], e["kind"]))
    return {"artifact_version": ARTIFACT_VERSION, "crate": crate, "items": items}


def main(argv):
    args = [a for a in argv[1:] if a != "--out"]
    out_path = None
    if "--out" in argv:
        i = argv.index("--out")
        if i + 1 >= len(argv):
            print("NO VERDICT: --out needs a path", file=sys.stderr)
            return NO_VERDICT
        out_path = argv[i + 1]
        args = [a for a in args if a != out_path]
    if len(args) != 2:
        print(
            "usage: extract_contracts.py <rustdoc.json> <crate-name> [--out <path>]",
            file=sys.stderr,
        )
        return NO_VERDICT
    json_path, crate = args

    try:
        doc = load(json_path, "contract", SUPPORTED_FORMAT_VERSIONS)
        artifact = extract(doc, crate)
    except (Unrecognised, ContractParseError) as e:
        print("NO VERDICT: %s" % e, file=sys.stderr)
        return NO_VERDICT
    except RecursionError:
        print("NO VERDICT: the public-surface walk did not terminate", file=sys.stderr)
        return NO_VERDICT

    if not artifact["items"]:
        print(
            "NO VERDICT: 0 contracts were extracted from %s. An extractor that "
            "found nothing has not verified anything (#5620). Either the crate "
            "has no `# Code Contract` blocks yet — in which case do not run this gate "
            "on it — or the walk did not reach them." % json_path,
            file=sys.stderr,
        )
        return NO_VERDICT

    text = json.dumps(artifact, indent=2, sort_keys=True, ensure_ascii=False) + "\n"
    if out_path:
        with open(out_path, "w") as fh:
            fh.write(text)
    else:
        sys.stdout.write(text)
    # The positive-evidence marker the shell requires before reporting a clean
    # run, the same rule check_semver_types.sh applies to its differ.
    print("extracted: %d contract(s)" % len(artifact["items"]), file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
