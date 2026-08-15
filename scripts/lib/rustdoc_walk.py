"""Shared rustdoc-JSON loader, type renderer, and public-surface walker.

Why: two gates now read the same rustdoc JSON and walk the same public surface —
  `check_semver_types.sh` (PR #5723) compares TYPE positions, and
  `extract_contracts.py` (#5724) extracts the `# Contract` blocks from doc
  comments. This repo's common-entry-point rule makes a second, independent
  traversal a defect rather than a convenience: the walk decides what counts as
  "the public surface", and two copies of that judgement drift silently. The
  visibility rules, the `pub use` re-export handling, the synthetic/blanket-impl
  skip, and the fail-closed posture are stated once, here.

What: `load()` reads and validates a rustdoc document. `render_type()` and its
  helpers render a `Type` node to a canonical string. `SurfaceWalker` walks the
  public surface from the crate root and YIELDS one `(kind, qual, item)` triple
  per public item position it reaches; each consumer decides what to record.
  The `qual` strings are built here so two consumers naming the same position
  always spell it the same way.

FAILING CLOSED IS THE WHOLE POINT, and it is inherited by every consumer.
  rustdoc JSON's schema is unstable and moves with the toolchain, so "I did not
  understand this document" is a normal outcome and must never render as
  "nothing changed" or "no contracts here". Every unrecognised shape raises
  `Unrecognised`; rendering an unknown node as a placeholder would make two
  different unknowns compare equal, which is the one way a differ built on this
  could report a false clean.

Format versions: each CONSUMER declares the versions it understands and passes
  them to `load()`. They deliberately differ — the semver gate reads documents
  built by cargo-semver-checks' pinned nightly, the contract extractor reads
  documents built by the repo's own nightly. A shared constant would force one
  to accept a schema it has not been checked against.

Test: `scripts/check_semver_types_selftest.sh` (the type differ's fixtures pin
  the traversal against a real 9-break probe crate) and
  `scripts/check_contracts_selftest.sh` (the extractor's fail-closed branches).
"""

import json

__all__ = [
    "Unrecognised",
    "load",
    "render_type",
    "render_path",
    "render_args",
    "render_arg",
    "render_constraint",
    "render_bound",
    "SurfaceWalker",
]


class Unrecognised(Exception):
    """A node, id, or document shape this code has not been checked against.

    Always fatal to the caller. Never caught and turned into a default.
    """


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


class SurfaceWalker:
    """Walks the public surface from the crate root, yielding item positions.

    Why: what counts as "public surface" is a judgement — whether a `pub use`
      of a foreign item is this crate's to keep, whether a blanket impl is this
      crate's API, whether a non-public field inside a public struct is
      reachable. Two consumers answering it separately would disagree over time
      and neither would be wrong on its own terms. Stated once here.
    What: `walk()` is a generator of `(kind, qual, item)` triples, where `kind`
      is one of the labels below and `qual` is the display path THIS class
      builds, so two consumers naming one position spell it identically:

        "function"     qual = the callable's path; item is the fn item
        "constant"     qual = the const's path
        "static"       qual = the static's path
        "type_alias"   qual = the alias's path
        "field"        qual = "<owner>.<field name>"
        "index_field"  qual = "<owner>.<index>"   (tuple struct / tuple variant)
        "assoc_const"  qual = "<owner>::<name>"
        "assoc_type"   qual = "<owner>::<name>"
        "struct" / "enum" / "union" / "trait" / "module"
                       qual = the item's own path, yielded BEFORE its children
                       so a consumer can contract a TYPE, not only its methods

      Traversal is depth-first and each (id, path) pair is visited once, so a
      cycle through re-exports terminates.
    Test: `scripts/check_semver_types_selftest.sh` walks the probe fixtures;
      `scripts/check_contracts_selftest.sh` covers the fail-closed branches.
    """

    def __init__(self, doc):
        self.index = doc["index"]
        self.root = doc["root"]
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

    def walk(self):
        root = self.item(self.root)
        if "module" not in root["inner"]:
            raise Unrecognised("crate root item is not a module")
        for triple in self.module(self.root, [root.get("name") or "crate"]):
            yield triple

    def module(self, mid, prefix):
        key = ("mod", str(mid), "::".join(prefix))
        if key in self.seen:
            return
        self.seen.add(key)
        for cid in self.item(mid)["inner"]["module"]["items"]:
            for triple in self.child(cid, prefix):
                yield triple

    def child(self, cid, prefix):
        it = self.item(cid)
        inner = it["inner"]
        kind = next(iter(inner))
        if kind == "use":
            for triple in self.reexport(inner["use"], prefix):
                yield triple
            return
        name = it.get("name")
        if not self.public(it) or name is None:
            return
        for triple in self.emit(kind, it, cid, prefix + [name]):
            yield triple

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
                for triple in self.module(tid, prefix):
                    yield triple
            return
        key = ("use", str(tid), "::".join(prefix), u.get("name") or "")
        if key in self.seen:
            return
        self.seen.add(key)
        inner = it["inner"]
        path = prefix + [u.get("name") or it.get("name") or "?"]
        for triple in self.emit(next(iter(inner)), it, tid, path):
            yield triple

    def emit(self, kind, it, iid, path):
        inner = it["inner"]
        qual = "::".join(path)
        if kind == "module":
            yield ("module", qual, it)
            for triple in self.module(iid, path):
                yield triple
        elif kind == "function":
            yield ("function", qual, it)
        elif kind in ("constant", "static", "type_alias"):
            yield (kind, qual, it)
        elif kind == "struct":
            yield ("struct", qual, it)
            for triple in self.struct(path, inner["struct"]):
                yield triple
        elif kind == "enum":
            yield ("enum", qual, it)
            for triple in self.enum(path, inner["enum"]):
                yield triple
        elif kind == "union":
            yield ("union", qual, it)
            for triple in self.union(path, inner["union"]):
                yield triple
        elif kind == "trait":
            yield ("trait", qual, it)
            for triple in self.trait(path, inner["trait"]):
                yield triple

    def struct(self, path, st):
        kind = st["kind"]
        if isinstance(kind, dict) and "plain" in kind:
            for fid in kind["plain"]["fields"]:
                for triple in self.named_field(fid, path):
                    yield triple
        elif isinstance(kind, dict) and "tuple" in kind:
            for i, fid in enumerate(kind["tuple"]):
                for triple in self.positional_field(fid, path, i):
                    yield triple
        for triple in self.impls(path, st.get("impls", [])):
            yield triple

    def union(self, path, un):
        for fid in un.get("fields", []):
            for triple in self.named_field(fid, path):
                yield triple
        for triple in self.impls(path, un.get("impls", [])):
            yield triple

    def named_field(self, fid, path):
        f = self.item(fid)
        if not self.public(f):
            return
        yield ("field", "%s.%s" % ("::".join(path), f.get("name")), f)

    def positional_field(self, fid, path, i):
        if fid is None:  # a stripped (non-public) tuple field
            return
        f = self.item(fid)
        if not self.public(f):
            return
        yield ("index_field", "%s.%d" % ("::".join(path), i), f)

    def enum(self, path, en):
        for vid in en.get("variants", []):
            v = self.item(vid)
            vk = v["inner"]["variant"]["kind"]
            vpath = path + [v.get("name") or "?"]
            yield ("variant", "::".join(vpath), v)
            if isinstance(vk, dict) and "tuple" in vk:
                for i, fid in enumerate(vk["tuple"]):
                    for triple in self.positional_field(fid, vpath, i):
                        yield triple
            elif isinstance(vk, dict) and "struct" in vk:
                for fid in vk["struct"]["fields"]:
                    f = self.item(fid)
                    yield ("field", "%s.%s" % ("::".join(vpath), f.get("name")), f)
        for triple in self.impls(path, en.get("impls", [])):
            yield triple

    def trait(self, path, tr):
        for iid in tr.get("items", []):
            for triple in self.assoc(self.item(iid), "::".join(path)):
                yield triple

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
                    for triple in self.assoc(it, owner):
                        yield triple
                else:
                    # Qualified, because an inherent `new` and a trait `new` are
                    # two different callables on the same type.
                    for triple in self.assoc(it, "<%s as %s>" % (owner, render_path(tr))):
                        yield triple

    def assoc(self, it, owner):
        inner = it["inner"]
        kind = next(iter(inner))
        qual = "%s::%s" % (owner, it.get("name") or "?")
        if kind == "function":
            yield ("function", qual, it)
        elif kind == "assoc_const":
            yield ("assoc_const", qual, it)
        elif kind == "assoc_type":
            yield ("assoc_type", qual, it)


# --- loader ----------------------------------------------------------------


def load(path, label, supported_format_versions):
    """Read a rustdoc JSON document, refusing anything unverified.

    Why: every downstream conclusion rests on the document being what it claims.
      A truncated file, a non-rustdoc JSON blob, or a schema this code has not
      been checked against would each produce a confident and wrong answer.
    What: parses `path`, requires `index`/`root`, and requires `format_version`
      to be in `supported_format_versions` — a tuple the CALLER supplies,
      because the two consumers read documents built by different toolchains.
      Raises `Unrecognised` on every failure; never returns a partial document.
    Test: the `bad-format`, `not-rustdoc`, `malformed` and missing-file cases in
      both selftests.
    """
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
    if fv not in supported_format_versions:
        raise Unrecognised(
            "%s rustdoc JSON %s has format_version %r; this reader understands %s. "
            "Add the version only after checking the node shapes it changed."
            % (label, path, fv, list(supported_format_versions))
        )
    return doc
