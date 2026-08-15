#!/usr/bin/env bash
#
# check_semver_types.sh — type-level public-API differ, for the gap
# cargo-semver-checks leaves open.
#
# Why: cargo-semver-checks 0.50.0 DOES NOT COMPARE TYPES. Its lints ask whether
#   an item still exists at its path, whether its kind still matches, whether its
#   parameter and generic COUNTS still match, and what its attributes and trait
#   impls are. Substitute any type and the item still exists, with the same name
#   and the same arity, so every lint passes. Measured against a 9-break probe
#   crate at `--release-type patch`, the strictest setting the tool has: 2 caught,
#   7 missed. All 7 misses were type substitutions — method return type, method
#   parameter type, free-fn return, free-fn parameter, struct field, `pub const`,
#   trait-method return. The tool's only return-type lints concern the `()`
#   boundary specifically.
#
#   The real-world instance is trusty-common 0.32.0 -> 0.33.0, which changed
#   `KgStoreRedb::count_active_triples` from `u64` to `Result<u64>` and the
#   `KnowledgeGraph` wrapper from `usize` to `Result<usize>`. Both items are
#   `visibility: public`, in a fully public module chain, with `memory-core`
#   enabled and no exclusion touching either. The gate reported
#   `196 checks: 196 pass, 0 failures`. The tool had the delta in its rustdoc
#   JSON and had no lint that looks at it.
#
# What: reads two rustdoc JSON documents, walks the PUBLIC surface of each from
#   the crate root, and reports every item that exists in both but whose type
#   RENDERS differently. Covered:
#     - every public fn/method — each parameter type, and the return type, keyed
#       separately so a report names the position that moved
#     - inherent-impl methods, trait-impl methods, and trait-definition methods
#     - public struct fields, tuple-struct fields, enum-variant fields
#     - public `const` and `static` types
#     - `type` aliases, and associated consts/types
#
# What it deliberately does NOT do:
#     - ADDED and REMOVED items are counted, never failed on. That is
#       cargo-semver-checks' half of the job and it does it correctly; failing
#       here too would double-report the 2 cases it already catches.
#     - Generic parameters, bounds, and where-clauses are not compared. A bound
#       change renders no differently in a signature and needs the trait graph.
#     - Lifetimes are rendered but not normalised, so a lifetime RENAME reports
#       as a change. It is a signature difference; it is not a false positive.
#     - Nothing behavioural. A precondition that changed under an unchanged
#       signature is invisible to any static differ, this one included — the
#       trusty-mpm `latest_trusty_mpm_snapshot` shape is the instance on record.
#
# FAILING CLOSED IS THE WHOLE POINT. rustdoc JSON's schema is unstable and
#   changes with the toolchain, so "I did not understand this document" is a
#   normal outcome and must never render as "nothing changed". Every one of these
#   exits NO VERDICT (3), naming the cause:
#     - either file is missing, unreadable, or not JSON
#     - `format_version` is absent, or not in SUPPORTED_FORMAT_VERSIONS below
#     - the two documents disagree on `format_version` (comparing across schema
#       versions compares rendering differences, not API differences)
#     - any type, generic-argument or bound node whose shape this differ does not
#       recognise — it raises rather than rendering the node as a placeholder,
#       because two unrecognised nodes rendered identically would read as "same"
#     - an item id referenced by the index but absent from it
#     - ZERO public items compared. #5620 is this repo's own instance of a gate
#       printing [PASS] over a comparison that never happened; a differ that
#       walked nothing has not agreed with anything.
#   The shell also requires POSITIVE EVIDENCE from the helper — the
#   `compared: N public item(s)` marker with N >= 1 — before it will report a
#   clean run, so a future refactor that breaks the helper turns this red rather
#   than green.
#
# Where the input comes from: the rustdoc JSON is ALREADY BUILT by the existing
#   gate and cached under `target/semver-checks/`, so `--crate` costs a JSON
#   parse and no rustdoc build. Run `bash scripts/check_semver.sh --crate <c>`
#   first if the cache is cold; this script never builds anything, and a cache
#   miss is a NO VERDICT with that command in the message, never a pass.
#
# Usage:
#   bash scripts/check_semver_types.sh --crate trusty-common
#   bash scripts/check_semver_types.sh --crate trusty-common --baseline 0.32.0
#   bash scripts/check_semver_types.sh --baseline-json <a.json> --current-json <b.json>
#
#   --crate accepts either the package name (`tga`) or the crates/ directory
#   name (`trusty-git-analytics`), the same two forms check_semver.sh accepts.
#   --cache-root <dir>  look for the semver-checks cache under <dir> instead of
#                       <repo>/target/semver-checks. A self-test seam.
#
# Exit (same vocabulary as check_semver.sh, so the two read alike):
#   0  compared at least one public item; no type changed.
#   1  A COMPARISON HAPPENED AND IT FOUND TYPE CHANGES. Each is listed.
#   2  usage error.
#   3  NO VERDICT — nothing was compared, so nothing may be concluded.
#
# Test: `scripts/check_semver_types_selftest.sh`. Its fixtures are the real
#   rustdoc JSON of the 9-break probe crate this script exists because of, so the
#   7 missed substitutions are pinned as regression tests against the tool that
#   missed them; the fail-closed branches above each have a case.
#
# Portability: bash 3.2 (macOS system bash) and bash 5 (Linux CI). POSIX tools
# plus `git`, `cargo` (only for --crate) and `python3`.

set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

EXIT_TYPE_CHANGE=1
EXIT_USAGE=2
EXIT_NO_VERDICT=3

CRATE=""
BASELINE_VERSION=""
BASELINE_JSON=""
CURRENT_JSON=""
CACHE_ROOT=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --crate)
      [[ $# -lt 2 ]] && {
        echo "ERROR: --crate needs a package name" >&2
        exit "$EXIT_USAGE"
      }
      CRATE="$2"
      shift 2
      ;;
    --baseline)
      [[ $# -lt 2 ]] && {
        echo "ERROR: --baseline needs a version" >&2
        exit "$EXIT_USAGE"
      }
      BASELINE_VERSION="$2"
      shift 2
      ;;
    --baseline-json)
      [[ $# -lt 2 ]] && {
        echo "ERROR: --baseline-json needs a path" >&2
        exit "$EXIT_USAGE"
      }
      BASELINE_JSON="$2"
      shift 2
      ;;
    --current-json)
      [[ $# -lt 2 ]] && {
        echo "ERROR: --current-json needs a path" >&2
        exit "$EXIT_USAGE"
      }
      CURRENT_JSON="$2"
      shift 2
      ;;
    --cache-root)
      [[ $# -lt 2 ]] && {
        echo "ERROR: --cache-root needs a directory" >&2
        exit "$EXIT_USAGE"
      }
      CACHE_ROOT="$2"
      shift 2
      ;;
    -h | --help)
      # Prints the contiguous comment block after the shebang, so editing the
      # header above never silently truncates --help against a stale line range.
      awk 'NR > 1 && /^#/ { print; next } NR > 1 { exit }' "$0" >&2
      exit 0
      ;;
    *)
      echo "ERROR: unknown argument '$1'" >&2
      exit "$EXIT_USAGE"
      ;;
  esac
done

if [[ -n "$CRATE" && (-n "$BASELINE_JSON" || -n "$CURRENT_JSON") ]]; then
  echo "ERROR: --crate and --baseline-json/--current-json are alternatives, not a pair." >&2
  exit "$EXIT_USAGE"
fi
if [[ -z "$CRATE" && (-z "$BASELINE_JSON" || -z "$CURRENT_JSON") ]]; then
  echo "ERROR: give either --crate <name>, or BOTH --baseline-json and --current-json." >&2
  exit "$EXIT_USAGE"
fi

SCRATCH="$(mktemp -d "${TMPDIR:-/tmp}/semver-types.XXXXXX")"
trap 'rm -rf "$SCRATCH"' EXIT
PY_HELPER="${SCRATCH}/typediff.py"

# ---------------------------------------------------------------------------
# The differ. Split out as a file rather than a pipeline so `python3 <file>` can
# take the two JSON paths as arguments — `python3 -` reads its program from
# stdin, and a heredoc and a pipe cannot both feed it.
# ---------------------------------------------------------------------------
cat > "$PY_HELPER" <<'PY'
"""Type-level rustdoc-JSON differ for check_semver_types.sh.

Exits 0 (clean), 1 (type changes, listed on stdout) or 3 (no verdict, reason on
stderr). Every unrecognised shape raises Unrecognised, which becomes exit 3 —
rendering an unknown node as a placeholder would make two different unknowns
compare equal, which is the one way this could report a false clean.
"""

import json
import sys

# Bump ONLY after re-reading the rustdoc-types changelog for the versions being
# added and confirming the node shapes below still hold. An unlisted version is
# a NO VERDICT, which is the safe direction: a schema this differ half-
# understands would compare rendering differences and call them API changes.
#
# 61 was added because the list said (57,) while every rustdoc on the machine
# emitted 61, so `--crate <anything>` exited 3 and this differ compared nothing
# on any real crate. It read only its own committed format-57 fixtures, so its
# self-test stayed green throughout and nothing said the tool was inert. The
# format-61 fixture pair exists so that cannot recur silently — see
# scripts/test-data/semver-types/README.md.
#
# THE STALENESS IS STRUCTURAL, not fixed by this entry. A frozen fixture proves
# the differ still reads the version it was captured at; it can never notice
# that the toolchain has moved past it. Only running the differ on rustdoc JSON
# the CURRENT toolchain produced can do that, and nothing does — see
# docs/reference/semver-gate.md, "The staleness this cannot detect".
SUPPORTED_FORMAT_VERSIONS = (57, 61)

NO_VERDICT = 3


class Unrecognised(Exception):
    pass


# --- type rendering --------------------------------------------------------
#
# Renders a rustdoc `Type` node to a canonical string. Item ids are NEVER part
# of the rendering: they are assigned per build and would differ between two
# documents of identical source.


def render_type(t):
    if t is None:
        return "()"  # rustdoc writes a missing return type as null
    if isinstance(t, str):
        if t == "infer":
            return "_"
        raise Unrecognised("type node (bare string) %r" % t)
    if not isinstance(t, dict) or len(t) != 1:
        raise Unrecognised("type node %r" % (sorted(t) if isinstance(t, dict) else t,))
    kind, body = next(iter(t.items()))
    if kind == "primitive" or kind == "generic":
        return body
    if kind == "resolved_path":
        return body["path"] + render_args(body.get("args"))
    if kind == "borrowed_ref":
        return "&%s%s" % ("mut " if body.get("is_mutable") else "", render_type(body["type"]))
    if kind == "raw_pointer":
        return "*%s %s" % ("mut" if body.get("is_mutable") else "const", render_type(body["type"]))
    if kind == "tuple":
        return "(%s)" % ", ".join(render_type(x) for x in body)
    if kind == "slice":
        return "[%s]" % render_type(body)
    if kind == "array":
        return "[%s; %s]" % (render_type(body["type"]), body.get("len"))
    if kind == "pat":
        return "%s is <pattern>" % render_type(body["type"])
    if kind == "impl_trait":
        return "impl %s" % " + ".join(render_bound(b) for b in body)
    if kind == "dyn_trait":
        parts = [render_path(p["trait"]) for p in body.get("traits", [])]
        if body.get("lifetime"):
            parts.append(body["lifetime"])
        return "dyn %s" % " + ".join(parts)
    if kind == "qualified_path":
        base = render_type(body["self_type"])
        tr = body.get("trait")
        if tr:
            base = "<%s as %s>" % (base, render_path(tr))
        return "%s::%s%s" % (base, body["name"], render_args(body.get("args")))
    if kind == "function_pointer":
        sig = body["sig"]
        return "fn(%s) -> %s" % (
            ", ".join(render_type(p[1]) for p in sig["inputs"]),
            render_type(sig.get("output")),
        )
    raise Unrecognised("type variant %r" % kind)


def render_path(p):
    return p["path"] + render_args(p.get("args"))


def render_args(a):
    if a is None:
        return ""
    if isinstance(a, str):
        if a == "return_type_notation":
            return "(..)"
        raise Unrecognised("generic-args node (bare string) %r" % a)
    kind, body = next(iter(a.items()))
    if kind == "angle_bracketed":
        parts = [render_arg(x) for x in body.get("args", [])]
        for c in body.get("constraints", []):
            parts.append(c["name"] + render_constraint(c))
        return "<%s>" % ", ".join(parts) if parts else ""
    if kind == "parenthesized":
        return "(%s) -> %s" % (
            ", ".join(render_type(x) for x in body.get("inputs", [])),
            render_type(body.get("output")),
        )
    if kind == "return_type_notation":
        return "(..)"
    raise Unrecognised("generic-args variant %r" % kind)


def render_arg(x):
    if isinstance(x, str):
        if x == "infer":
            return "_"
        raise Unrecognised("generic-arg node (bare string) %r" % x)
    kind, body = next(iter(x.items()))
    if kind == "lifetime":
        return body
    if kind == "type":
        return render_type(body)
    if kind == "const":
        return str(body.get("expr"))
    if kind == "infer":
        return "_"
    raise Unrecognised("generic-arg variant %r" % kind)


def render_constraint(c):
    b = c.get("binding")
    if b is None:
        return ""
    if isinstance(b, str):
        raise Unrecognised("assoc-item constraint (bare string) %r" % b)
    kind, body = next(iter(b.items()))
    if kind == "equality":
        if "type" in body:
            return " = %s" % render_type(body["type"])
        if "constant" in body:
            return " = %s" % body["constant"].get("expr")
        raise Unrecognised("equality binding %r" % sorted(body))
    if kind == "constraint":
        return ": %s" % " + ".join(render_bound(x) for x in body)
    raise Unrecognised("assoc-item-constraint variant %r" % kind)


def render_bound(b):
    if isinstance(b, str):
        raise Unrecognised("generic-bound node (bare string) %r" % b)
    kind, body = next(iter(b.items()))
    if kind == "trait_bound":
        return render_path(body["trait"])
    if kind == "outlives":
        return body
    if kind == "use":
        return "use<..>"
    raise Unrecognised("generic-bound variant %r" % kind)


# --- surface walk ----------------------------------------------------------
#
# Walks from the crate root through public modules, collecting one entry per
# TYPE POSITION rather than per item, so a report names which parameter moved
# instead of printing two whole signatures for the reader to diff by eye.


class Walker:
    def __init__(self, doc):
        self.index = doc["index"]
        self.root = doc["root"]
        self.out = {}
        self.seen = set()

    def item(self, iid):
        it = self.index.get(str(iid))
        if it is None and not isinstance(iid, str):
            it = self.index.get(iid)
        if it is None:
            raise Unrecognised("item id %r is referenced but absent from the index" % (iid,))
        return it

    @staticmethod
    def public(it):
        return it.get("visibility") == "public"

    def run(self):
        root = self.item(self.root)
        if "module" not in root["inner"]:
            raise Unrecognised("crate root item is not a module")
        self.module(self.root, [root.get("name") or "crate"])
        return self.out

    def module(self, mid, prefix):
        key = ("mod", str(mid), "::".join(prefix))
        if key in self.seen:
            return
        self.seen.add(key)
        for cid in self.item(mid)["inner"]["module"]["items"]:
            self.child(cid, prefix)

    def child(self, cid, prefix):
        it = self.item(cid)
        inner = it["inner"]
        kind = next(iter(inner))
        if kind == "use":
            self.reexport(inner["use"], prefix)
            return
        name = it.get("name")
        if not self.public(it) or name is None:
            return
        self.emit(kind, inner, cid, prefix + [name])

    def reexport(self, u, prefix):
        """`pub use` — part of the public surface, and often the only path to it.

        A re-export of a foreign crate's item is not this crate's surface to
        keep, so it is passed over rather than treated as an unknown shape.
        """
        tid = u.get("id")
        if tid is None:
            return
        it = self.index.get(str(tid)) or (
            self.index.get(tid) if not isinstance(tid, str) else None
        )
        if it is None or it.get("crate_id") != 0:
            return
        if u.get("is_glob"):
            if "module" in it["inner"]:
                self.module(tid, prefix)
            return
        key = ("use", str(tid), "::".join(prefix), u.get("name") or "")
        if key in self.seen:
            return
        self.seen.add(key)
        inner = it["inner"]
        self.emit(next(iter(inner)), inner, tid, prefix + [u.get("name") or it.get("name") or "?"])

    def emit(self, kind, inner, iid, path):
        qual = "::".join(path)
        if kind == "module":
            self.module(iid, path)
        elif kind == "function":
            self.function(qual, inner["function"])
        elif kind == "constant":
            self.out["const %s" % qual] = render_type(inner["constant"]["type"])
        elif kind == "static":
            self.out["static %s" % qual] = render_type(inner["static"]["type"])
        elif kind == "type_alias":
            self.out["type %s" % qual] = render_type(inner["type_alias"]["type"])
        elif kind == "struct":
            self.struct(path, inner["struct"])
        elif kind == "enum":
            self.enum(path, inner["enum"])
        elif kind == "union":
            self.union(path, inner["union"])
        elif kind == "trait":
            self.trait(path, inner["trait"])

    def function(self, qual, fn):
        sig = fn["sig"]
        for i, pair in enumerate(sig["inputs"]):
            # Keyed by POSITION as well as name: renaming a parameter is not an
            # API change, but the position it sits in is what a caller passes.
            self.out["fn %s(#%d %s)" % (qual, i, pair[0])] = render_type(pair[1])
        self.out["fn %s -> " % qual] = render_type(sig.get("output"))

    def struct(self, path, st):
        kind = st["kind"]
        if isinstance(kind, dict) and "plain" in kind:
            for fid in kind["plain"]["fields"]:
                self.named_field(fid, path)
        elif isinstance(kind, dict) and "tuple" in kind:
            for i, fid in enumerate(kind["tuple"]):
                self.positional_field(fid, path, i)
        self.impls(path, st.get("impls", []))

    def union(self, path, un):
        for fid in un.get("fields", []):
            self.named_field(fid, path)
        self.impls(path, un.get("impls", []))

    def named_field(self, fid, path):
        f = self.item(fid)
        if not self.public(f):
            return
        self.out["field %s.%s" % ("::".join(path), f.get("name"))] = render_type(
            f["inner"]["struct_field"]
        )

    def positional_field(self, fid, path, i):
        if fid is None:  # a stripped (non-public) tuple field
            return
        f = self.item(fid)
        if not self.public(f):
            return
        self.out["field %s.%d" % ("::".join(path), i)] = render_type(f["inner"]["struct_field"])

    def enum(self, path, en):
        for vid in en.get("variants", []):
            v = self.item(vid)
            vk = v["inner"]["variant"]["kind"]
            vpath = path + [v.get("name") or "?"]
            if isinstance(vk, dict) and "tuple" in vk:
                for i, fid in enumerate(vk["tuple"]):
                    self.positional_field(fid, vpath, i)
            elif isinstance(vk, dict) and "struct" in vk:
                for fid in vk["struct"]["fields"]:
                    f = self.item(fid)
                    self.out["field %s.%s" % ("::".join(vpath), f.get("name"))] = render_type(
                        f["inner"]["struct_field"]
                    )
        self.impls(path, en.get("impls", []))

    def trait(self, path, tr):
        for iid in tr.get("items", []):
            self.assoc(self.item(iid), "::".join(path))

    def impls(self, path, impl_ids):
        """Inherent and real trait impls. Synthetic and blanket impls are skipped.

        Both are rustdoc's rendering of impls the crate never wrote — auto
        traits, and every `impl<T: Display> ToString for T` in core — so they
        move with the toolchain rather than with this crate's API.
        """
        owner = "::".join(path)
        for iid in impl_ids:
            im = self.item(iid)["inner"]["impl"]
            if im.get("is_synthetic") or im.get("blanket_impl"):
                continue
            tr = im.get("trait")
            for mid in im.get("items", []):
                it = self.item(mid)
                if tr is None:
                    if not self.public(it):
                        continue
                    self.assoc(it, owner)
                else:
                    # Qualified, because an inherent `new` and a trait `new` are
                    # two different callables on the same type.
                    self.assoc(it, "<%s as %s>" % (owner, render_path(tr)))

    def assoc(self, it, owner):
        inner = it["inner"]
        kind = next(iter(inner))
        qual = "%s::%s" % (owner, it.get("name") or "?")
        if kind == "function":
            self.function(qual, inner["function"])
        elif kind == "assoc_const":
            self.out["assoc const %s" % qual] = render_type(inner["assoc_const"]["type"])
        elif kind == "assoc_type":
            t = inner["assoc_type"].get("type")
            if t is not None:
                self.out["assoc type %s" % qual] = render_type(t)


# --- driver ----------------------------------------------------------------


def load(path, label):
    try:
        with open(path) as fh:
            doc = json.load(fh)
    except OSError as e:
        raise Unrecognised("%s rustdoc JSON %s could not be read: %s" % (label, path, e))
    except ValueError as e:
        raise Unrecognised("%s rustdoc JSON %s did not parse: %s" % (label, path, e))
    if not isinstance(doc, dict) or "index" not in doc or "root" not in doc:
        raise Unrecognised("%s file %s is JSON but not a rustdoc document" % (label, path))
    fv = doc.get("format_version")
    if fv not in SUPPORTED_FORMAT_VERSIONS:
        raise Unrecognised(
            "%s rustdoc JSON %s has format_version %r; this differ understands %s. "
            "Add the version to SUPPORTED_FORMAT_VERSIONS only after checking the "
            "node shapes it changed." % (label, path, fv, list(SUPPORTED_FORMAT_VERSIONS))
        )
    return doc


def main(argv):
    if len(argv) != 3:
        print("usage: typediff.py <baseline.json> <current.json>", file=sys.stderr)
        return NO_VERDICT
    base_path, cur_path = argv[1], argv[2]
    try:
        base_doc = load(base_path, "baseline")
        cur_doc = load(cur_path, "current")
        if base_doc["format_version"] != cur_doc["format_version"]:
            raise Unrecognised(
                "the two documents were written by different rustdoc JSON schemas "
                "(%r vs %r); differences between them would be rendering, not API"
                % (base_doc["format_version"], cur_doc["format_version"])
            )
        base = Walker(base_doc).run()
        cur = Walker(cur_doc).run()
    except Unrecognised as e:
        print("NO VERDICT: %s" % e, file=sys.stderr)
        return NO_VERDICT
    except RecursionError:
        print("NO VERDICT: the public-surface walk did not terminate", file=sys.stderr)
        return NO_VERDICT

    common = sorted(set(base) & set(cur))
    changed = [k for k in common if base[k] != cur[k]]
    removed = len(set(base) - set(cur))
    added = len(set(cur) - set(base))

    if not common:
        print(
            "NO VERDICT: 0 public items are present in both documents, so nothing "
            "was compared. A differ that walked nothing has not agreed with "
            "anything (#5620).",
            file=sys.stderr,
        )
        return NO_VERDICT

    for k in changed:
        print("CHANGED %s: %s -> %s" % (k, base[k], cur[k]))
    # The marker line the shell requires as positive evidence. Its count is the
    # number of type positions present on BOTH sides — the population that could
    # have disagreed.
    print(
        "compared: %d public item(s); %d changed, %d removed, %d added"
        % (len(common), len(changed), removed, added)
    )
    return 1 if changed else 0


sys.exit(main(sys.argv))
PY

# ---------------------------------------------------------------------------
# --crate: find the two documents the existing gate already built.
#
# Layout, as cargo-semver-checks 0.50.0 writes it under target/semver-checks/:
#   baseline  cache/<crate_us>-<ver_us>-<target>-<hash>.json
#   current   local-<crate_us>-<ver_us>-<target>-<hash>/target/doc/<crate_us>.json
# The hash covers the feature set, so two entries for one version mean two
# different feature sets. Comparing across those compares feature availability,
# not API, so an ambiguous match is refused rather than guessed at.
# ---------------------------------------------------------------------------
#
# Sets PICKED / PICK_COUNT / PICK_LIST rather than echoing them. A command
# substitution is a SUBSHELL, so an `exit` inside one ends only that subshell and
# the caller carries on — the same trap check_semver.sh's registry_probe
# documents. Here it would have printed the ambiguity refusal AND the cold-cache
# remedy for the same run.
PICKED=""
PICK_COUNT=0
PICK_LIST=""
pick_one() {
  local f
  PICKED=""
  PICK_COUNT=0
  PICK_LIST=""
  for f in "$@"; do
    # The empty element is what `"${arr[@]:-}"` expands to for an empty array
    # under bash 3.2; the sidecar is cargo-semver-checks' own metadata file,
    # which sits beside every cached baseline and matches the same glob.
    [[ -z "$f" || ! -f "$f" ]] && continue
    case "$f" in
      *.metadata.json) continue ;;
    esac
    PICK_COUNT=$((PICK_COUNT + 1))
    PICKED="$f"
    PICK_LIST="${PICK_LIST}            ${f}"$'\n'
  done
}

refuse_ambiguous() {
  {
    echo "NO VERDICT: ${PICK_COUNT} cached ${1} documents match, so which pair to compare is"
    echo "            ambiguous. They differ by feature set, and comparing across feature"
    echo "            sets compares availability rather than API:"
    printf '%s' "$PICK_LIST"
    echo "            Nothing was compared. Pass --baseline-json/--current-json to name"
    echo "            the pair."
  } >&2
  exit "$EXIT_NO_VERDICT"
}

if [[ -n "$CRATE" ]]; then
  META="${SCRATCH}/metadata.json"
  if ! cargo metadata --no-deps --format-version 1 > "$META" 2> "${SCRATCH}/meta-err.txt"; then
    echo "NO VERDICT: 'cargo metadata --no-deps' failed:" >&2
    sed 's/^/       /' "${SCRATCH}/meta-err.txt" >&2
    exit "$EXIT_NO_VERDICT"
  fi

  # Resolve package name and version. Accepts a crates/ directory name too, the
  # same two forms check_semver.sh accepts, so a release tag prefix works here.
  RESOLVED="$(META_FILE="$META" REPO="$REPO_ROOT" WANT="$CRATE" python3 -c '
import json, os
meta = json.load(open(os.environ["META_FILE"]))
want = os.environ["WANT"]
target = os.path.realpath(os.path.join(os.environ["REPO"], "crates", want, "Cargo.toml"))
for p in meta["packages"]:
    if p["name"] == want or os.path.realpath(p["manifest_path"]) == target:
        print(p["name"])
        print(p["version"])
        break
')"
  PKG="$(printf '%s\n' "$RESOLVED" | sed -n 1p)"
  PKG_VERSION="$(printf '%s\n' "$RESOLVED" | sed -n 2p)"
  if [[ -z "$PKG" ]]; then
    echo "NO VERDICT: '${CRATE}' is neither a workspace package name nor a crates/ directory." >&2
    exit "$EXIT_NO_VERDICT"
  fi

  CACHE_ROOT="${CACHE_ROOT:-${REPO_ROOT}/target/semver-checks}"
  PKG_US="$(printf '%s' "$PKG" | tr '-' '_')"
  CUR_US="$(printf '%s' "$PKG_VERSION" | tr '.' '_')"

  shopt -s nullglob
  # shellcheck disable=SC2206
  CUR_MATCHES=(${CACHE_ROOT}/local-${PKG_US}-${CUR_US}-*/target/doc/${PKG_US}.json)
  if [[ -n "$BASELINE_VERSION" ]]; then
    BASE_US="$(printf '%s' "$BASELINE_VERSION" | tr '.' '_')"
    # shellcheck disable=SC2206
    BASE_MATCHES=(${CACHE_ROOT}/cache/${PKG_US}-${BASE_US}-*.json)
  else
    # shellcheck disable=SC2206
    BASE_MATCHES=(${CACHE_ROOT}/cache/${PKG_US}-*.json)
  fi
  shopt -u nullglob

  pick_one "${CUR_MATCHES[@]:-}"
  # The remedy above is a dead end for a crate check_semver.sh SKIPS: every skip
  # branch `continue`s before invoking cargo-semver-checks, so re-running the
  # gate builds nothing and the operator loops. Say so rather than let them find
  # out by repeating the command.
  cold_cache_caveat() {
    echo "            If that command reports SKIP for ${PKG}, it will never populate"
    echo "            this cache — a TSV-excluded crate, one with publish = false, no"
    echo "            library target, or no baseline on crates.io is skipped before any"
    echo "            rustdoc is built. Comparing its types needs two documents built"
    echo "            by hand and passed directly:"
    echo "              bash ${0##*/} --baseline-json <a.json> --current-json <b.json>"
  }

  if [[ "$PICK_COUNT" -eq 0 ]]; then
    {
      echo "NO VERDICT: no cached rustdoc JSON for ${PKG} ${PKG_VERSION} under"
      echo "            ${CACHE_ROOT}/local-${PKG_US}-${CUR_US}-*/target/doc/${PKG_US}.json"
      echo "            Nothing was compared. Build the cache first — it is the same"
      echo "            rustdoc the existing gate needs anyway:"
      echo "              bash scripts/check_semver.sh --crate ${PKG}"
      cold_cache_caveat
    } >&2
    exit "$EXIT_NO_VERDICT"
  fi
  if [[ "$PICK_COUNT" -gt 1 ]]; then
    refuse_ambiguous "current"
  fi
  CURRENT_JSON="$PICKED"

  pick_one "${BASE_MATCHES[@]:-}"
  if [[ "$PICK_COUNT" -eq 0 ]]; then
    {
      echo "NO VERDICT: no cached baseline rustdoc JSON for ${PKG}${BASELINE_VERSION:+ ${BASELINE_VERSION}} under"
      echo "            ${CACHE_ROOT}/cache/"
      echo "            Nothing was compared. Build the cache first:"
      echo "              bash scripts/check_semver.sh --crate ${PKG}"
      cold_cache_caveat
    } >&2
    exit "$EXIT_NO_VERDICT"
  fi
  if [[ "$PICK_COUNT" -gt 1 ]]; then
    refuse_ambiguous "baseline"
  fi
  BASELINE_JSON="$PICKED"
  LABEL="${PKG} ${PKG_VERSION}"
else
  # Not the basenames: both sides of a real comparison are usually called
  # `<crate>.json`, so a basename pair reads as the file compared with itself.
  LABEL="the file pair below"
fi

echo "TYPES ${LABEL}"
echo "  baseline: ${BASELINE_JSON}"
echo "  current:  ${CURRENT_JSON}"

RUN_LOG="${SCRATCH}/typediff.out"
rc=0
python3 "$PY_HELPER" "$BASELINE_JSON" "$CURRENT_JSON" > "$RUN_LOG" 2> "${SCRATCH}/typediff.err" || rc=$?
cat "$RUN_LOG"
cat "${SCRATCH}/typediff.err" >&2

# POSITIVE EVIDENCE, not a bare exit status — the same rule check_semver.sh's
# verdict_computed applies. A helper that crashed, was killed, or was refactored
# into printing nothing exits some status this cannot classify, and the only safe
# reading of "no marker" is that nothing was compared.
MARKER="$(grep -E '^compared: [0-9]+ public item\(s\);' "$RUN_LOG" | tail -1 || true)"
COMPARED=""
if [[ -n "$MARKER" ]]; then
  COMPARED="$(printf '%s\n' "$MARKER" | sed -nE 's/^compared: ([0-9]+) public item\(s\);.*/\1/p')"
fi

if [[ "$rc" -ne 0 && "$rc" -ne 1 ]] || [[ -z "$COMPARED" ]] || [[ "$COMPARED" -lt 1 ]]; then
  cat >&2 <<EOF

NO TYPE VERDICT WAS COMPUTED for ${LABEL}. This is NOT a pass: nothing is known
either way about whether a type moved.

The differ exited ${rc}$([[ -z "$COMPARED" ]] && echo " without printing its 'compared:' marker" || echo " having compared ${COMPARED} item(s)").
The usual causes, all of them deliberate refusals:

  * rustdoc JSON's format_version is not one this differ understands. The
    toolchain moved; read the rustdoc-types changelog for the new version and
    extend SUPPORTED_FORMAT_VERSIONS.
  * a type, generic-argument or bound node whose shape is unrecognised. The
    message above names it. Rendering it as a placeholder would make two
    different unknowns compare equal, so it refuses instead.
  * one of the two files is missing, truncated, or not a rustdoc document.

Fix the differ, then re-run. "It could not compare" is not "the types match".
EOF
  exit "$EXIT_NO_VERDICT"
fi

if [[ "$rc" -eq 1 ]]; then
  cat >&2 <<EOF

TYPE CHANGE(S) FOUND in the public API of ${LABEL}, listed above.

cargo-semver-checks does not compare types, so NONE of these will appear in its
output however strict --release-type is. Each one is a source-breaking change
for any caller that named the old type.

Confirm every one was intended. If the release is not already carrying a
breaking bump, it needs one — 0.x crates in the MINOR position, 1.x+ in MAJOR.
EOF
  exit "$EXIT_TYPE_CHANGE"
fi

echo "semver type differ: ${COMPARED} public item position(s) compared, 0 type change(s) — OK."
