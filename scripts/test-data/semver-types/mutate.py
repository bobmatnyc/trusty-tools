"""Derive a mutated rustdoc JSON from probe-base.json, for check_semver_types_selftest.sh.

Why a script and not more committed fixtures: each mutation below is one field
of a 51 KB document. Committing four near-identical copies would put 200 KB in
the repo and hide the mutation — the only interesting part — inside a minified
blob. Here the mutation IS the file, and the fail-closed case each one drives is
named beside it.

Usage:  python3 mutate.py <mode> <in.json> <out.json>

Modes:
  additive     add one public free fn. The clean-pair case has to be additive
               rather than identical, or it only proves the differ is
               deterministic.
  bad-format   set format_version to one no differ claims to understand.
  unknown-type stand in for the next rustdoc schema change: replace one return
               type with a Type variant that does not exist.
  empty        strip every item, so the two sides share no public surface and
               there is nothing to compare.
"""

import json
import sys


def additive(d):
    new_id = max(int(k) for k in d["index"]) + 1
    d["index"][str(new_id)] = {
        "id": new_id,
        "crate_id": 0,
        "name": "added_fn",
        "span": None,
        "visibility": "public",
        "docs": None,
        "links": {},
        "attrs": [],
        "deprecation": None,
        "inner": {
            "function": {
                "sig": {
                    "inputs": [["y", {"primitive": "u32"}]],
                    "output": {"primitive": "u32"},
                    "is_c_variadic": False,
                },
                "generics": {"params": [], "where_predicates": []},
                "header": {
                    "is_const": False,
                    "is_unsafe": False,
                    "is_async": False,
                    "abi": "Rust",
                },
                "has_body": True,
            }
        },
    }
    d["index"][str(d["root"])]["inner"]["module"]["items"].append(new_id)
    return d


def bad_format(d):
    d["format_version"] = 999999
    return d


def unknown_type(d):
    for it in d["index"].values():
        if it.get("name") == "free_ret" and "function" in it["inner"]:
            it["inner"]["function"]["sig"]["output"] = {
                "quantum_ref": {"type": {"primitive": "u64"}}
            }
            return d
    raise SystemExit("free_ret is not in this document; the fixture changed shape")


def empty(d):
    root = d["index"][str(d["root"])]
    root["inner"]["module"]["items"] = []
    d["index"] = {str(d["root"]): root}
    d["paths"] = {}
    return d


MODES = {
    "additive": additive,
    "bad-format": bad_format,
    "unknown-type": unknown_type,
    "empty": empty,
}

if len(sys.argv) != 4 or sys.argv[1] not in MODES:
    raise SystemExit("usage: mutate.py {%s} <in.json> <out.json>" % "|".join(sorted(MODES)))

with open(sys.argv[2]) as fh:
    doc = json.load(fh)
with open(sys.argv[3], "w") as fh:
    json.dump(MODES[sys.argv[1]](doc), fh, separators=(",", ":"), sort_keys=True)
