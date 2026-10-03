#!/usr/bin/env python3
"""check_asset_test_filter.py — the asset-content test filter list and its guard (#8378).

Why: an instruction-content edit under crates/*/src/assets/** is Cargo-inert
  (ADR-0064), so `Rust tests (affected crates)` plans nothing for it and the
  push-to-main shards skip it. Until PHASE_3 the tests that READ those assets
  run instead as a filtered `cargo test` step in capabilities-drift.yml (owner
  ruling 2026-09-27). The filter is a checked-in list. A test that reads an
  asset but sits outside the list would run nowhere on such a diff, and the
  next code push to main would go red on a commit that did not cause it.

What: three subcommands over scripts/asset-content-tests.tsv.
  check  the guard. Walks every crate's lib, bin and test targets from their
         module roots, finds each asset READ in test code, and fails when a
         list row does not cover it. It also fails on a malformed row or a row
         naming a target that does not exist.
  plan   prints the one cargo invocation per (crate, target) the list implies.
  run    executes the plan. It fails on a failing test, and on a row that
         selected no test (a renamed test must not pass vacuously).

  An ASSET here is a file whose edit is Cargo-inert instruction content: the
  set `scripts/detect-docs-only.sh --instruction-assets` accepts, the same
  predicate that sets capabilities-drift's `asset_content=true` (today the
  .md files under the trusty-mpm and trusty-agents-common asset roots, and
  content/**). A non-.md asset or trusty-code's compiled-in .md is code: an
  edit to it plans its crate in `Rust tests (affected crates)`, so its
  readers need no row.

  A test READS an asset when its test code holds:
    R1  include_str!/include_bytes!/include_dir! of a file that is an asset,
        or of a directory holding one;
    R2  a string literal naming a crates/<crate>/src/assets path that is an
        asset, a directory holding one, or a prefix of one:
        `<crate>/src/assets…`, a bare `src/assets…`, or `…content/…` (#9011);
    R3  a reference to an ASSET SYMBOL. Asset symbols are found by search, to
        a fixed point, in non-test code:
          - a const/static whose initializer holds R1, R2 or an asset symbol;
          - a loader: a fn whose body holds R1/R2 or an asset symbol, in a
            file that defines an asset const/static or holds R1 itself;
          - a `pub use` of either, which makes the re-exporting module a home.
        A reference counts when a home module qualifies it
        (`bundle::OUTPUT_STYLE`, `bundle::TM_WORKFLOW`, `super::X`), or when it is
        bare and in scope (`use …::home::{X}`, `use …::home::*`, a home's own
        module, or `use super::*` beneath one).
    R4  (#9011) a call to a run-time content loader: `checkout_content`,
        `AgentRoster::load` or `HarnessDoc::load` read the checkout's
        `content/` tree, which no literal names. A test-code helper fn that
        reads by R1-R4 (`repo_roster()`, `stage_repo_content()`) is itself a
        loader, homed in its module, so the TESTS calling it are the readers
        and the helper module needs no row of its own.
  `check --verbose` prints every symbol and loader it derived.

  Test code is a module under `#[cfg(test)]`, an integration-test target, or
  an item carrying `#[cfg(test)]` or a test attribute. A read inside a test fn
  needs a row covering that test. A read anywhere else in test code (a
  helper, a const) needs a row covering its whole module.

List format (TSV, `#` comments): crate, target, filter, reason.
  target: `lib`, `bin:<name>` or `test:<name>`.
  filter: `*` for the whole target, else a test-path prefix ending at a `::`
  boundary. cargo matches a filter as a substring, so a prefix selects at
  least the tests the guard counts it as covering.

Usage:
  scripts/check_asset_test_filter.py check [--root DIR] [--list FILE] [--verbose]
  scripts/check_asset_test_filter.py plan  [--root DIR] [--list FILE]
  scripts/check_asset_test_filter.py run   [--root DIR] [--list FILE]
Exit: 0 clean; 1 guard or run failure; 2 usage or unreadable input.
Test: scripts/check_asset_test_filter_selftest.sh.
"""
import argparse
import glob
import os
import re
import subprocess
import sys
import time
import tomllib

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "lib"))
from asset_filter_lex import attrs_before, expand_use, lex, match_close  # noqa: E402

IDENT = r"[A-Za-z_][A-Za-z0-9_]*"
INCLUDE = re.compile(r"\binclude_(?:str|bytes|dir)\s*!\s*\(")
MOD_DECL = re.compile(r"\bmod\s+(" + IDENT + r")\s*([;{])")
FN_DECL = re.compile(r"\bfn\s+(" + IDENT + r")\b")
ITEM_DECL = re.compile(r"\b(const|static)\s+(?:mut\s+)?(" + IDENT + r")\s*:")
USE_DECL = re.compile(r"\b(pub(?:\s*\([^)]*\))?\s+)?use\s+([^;]+);")
# A lookahead, so `crate::bundle::X` yields both `crate::bundle` and `bundle::X`.
QUALIFIED = re.compile(r"(?<!\w)(?=(" + IDENT + r")\s*::\s*(" + IDENT + r")\b)")
BARE = re.compile(r"(?<![\w:.])(" + IDENT + r")\b(?!\s*::)")
TEST_ATTR = re.compile(r"#\[\s*(?:" + IDENT + r"\s*::\s*)*(?:test|rstest|test_case)\b")
CFG_TEST = re.compile(r"#\[\s*cfg\s*\((?![^\]]*\bnot\s*\(\s*test)[^\]]*\btest\b")
# Group 1: a sibling crate's `src/assets`. Group 2 (#9011): the repo-root content/
# tree, which a crate names as `../../content/…` from its manifest dir.
ASSET_DIR = re.compile(r"(?:^|/)(?:(?:([\w.-]+)/)?src/assets(?:/|$)|(content)/)")
DETECT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "detect-docs-only.sh")
# R4 (#9011): (home, name) of the run-time readers of the content/ tree.
CONTENT_LOADERS = (("agent_content", "checkout_content"), ("AgentRoster", "load"), ("HarnessDoc", "load"))


# ------------------------------------------------------------- source files
class Source:
    """One lexed .rs file. Every span records its innermost inline module index."""

    def __init__(self, path, crate_dir, assets):
        with open(path, encoding="utf-8", errors="replace") as f:
            self.text = f.read()
        self.path, self.crate_dir, self.assets = path, crate_dir, assets
        self.masked, self.lits = lex(self.text)
        m = self.masked
        self.inline = []  # (start, end, name, attrs)
        decls = []
        for d in MOD_DECL.finditer(m):
            attrs = attrs_before(m, d.start(), self.text)
            if d.group(2) == "{":
                brace = d.end() - 1
                self.inline.append((brace, match_close(m, brace), d.group(1), attrs))
            else:
                decls.append((d.start(), d.group(1), attrs))
        self.decls = [(off, name, attrs, self.owner(off)) for off, name, attrs in decls]
        self.fns = []  # (start, end, name, attrs, owner)
        for d in FN_DECL.finditer(m):
            k, depth = d.end(), 0
            while k < len(m):
                ch = m[k]
                if ch in "([<":
                    depth += 1
                elif ch in ")]>" and not (ch == ">" and m[k - 1] == "-"):
                    depth -= 1
                elif depth <= 0 and ch in "{;":
                    break
                k += 1
            if k < len(m) and m[k] == "{":
                self.fns.append((d.start(), match_close(m, k), d.group(1),
                                 attrs_before(m, d.start(), self.text), self.owner(d.start())))
        self.items = []  # (start, end, name, attrs, owner) — const/static with initializer
        for d in ITEM_DECL.finditer(m):
            eq, semi = m.find("=", d.end()), m.find(";", d.end())
            if eq < 0 or (0 <= semi < eq):
                continue
            k, depth = eq, 0
            while k < len(m):
                ch = m[k]
                if ch in "([{":
                    depth += 1
                elif ch in ")]}":
                    depth -= 1
                elif ch == ";" and depth <= 0:
                    break
                k += 1
            self.items.append((d.start(), k, d.group(2), attrs_before(m, d.start(), self.text),
                               self.owner(d.start())))
        self.uses = []  # (offset, is_pub, [segments], alias, owner)
        code = list(m)
        for d in USE_DECL.finditer(m):
            for segs, alias in expand_use(d.group(2)):
                self.uses.append((d.start(), bool(d.group(1)), segs, alias, self.owner(d.start())))
            # An import names a symbol without reading it; references are counted in `code`.
            code[d.start():d.end()] = re.sub(r"[^\n]", " ", d.group(0))
        self.code = "".join(code)
        self._refs = {}

    def owner(self, off):
        """Index of the innermost inline module holding `off`, or None."""
        best = None
        for idx, (s, e, _, _) in enumerate(self.inline):
            if s < off < e and (best is None or s > self.inline[best][0]):
                best = idx
        return best

    def chain(self, idx):
        """Inline module indices from the outermost down to `idx`."""
        out = []
        while idx is not None:
            out.append(idx)
            s = self.inline[idx][0]
            idx = self.owner(s)
        return list(reversed(out))

    def line(self, off):
        return self.text.count("\n", 0, off) + 1

    def asset_literal(self, lit_off, value):
        """R1/R2 for one literal: the reason string, or None."""
        m = ASSET_DIR.search(value)
        if m:
            crate_dir = os.path.join(os.path.dirname(self.crate_dir), m.group(1)) if m.group(1) else self.crate_dir
            # A format string (`src/assets/agents/{name}.md`) names the prefix before its first hole.
            tail = re.split(r"[{}*?]", value[m.end():], maxsplit=1)[0]
            # #9011: group 2 is the repo-root content tree (`<manifest dir>/../../content/agents`).
            prefix = os.path.normpath(os.path.join(crate_dir, *(("..", "..", "content") if m.group(2) else ("src", "assets")), tail))
            if any(a.startswith(prefix) for a in self.assets):
                return f"literal {value!r}"
        if value and not value.startswith("/"):
            start = self.masked.rfind("include_", 0, lit_off)
            if start >= 0 and INCLUDE.match(self.masked, start):
                paren = self.masked.find("(", start)
                if lit_off < match_close(self.masked, paren):
                    resolved = os.path.normpath(os.path.join(os.path.dirname(self.path), value))
                    if any(a == resolved or a.startswith(resolved + os.sep) for a in self.assets):
                        return f"include of {value!r}"
        return None

    def refs(self, s, e):
        """(literal reason or None, [(qualifier, name)], {bare names}) for a span; cached."""
        key = (s, e)
        if key not in self._refs:
            lit = next((r for off, v in self.lits if s <= off < e for r in [self.asset_literal(off, v)] if r), None)
            text = self.code[s:e]
            quals = [(q.group(1), q.group(2)) for q in QUALIFIED.finditer(text)]
            self._refs[key] = (lit, quals, set(BARE.findall(text)))
        return self._refs[key]


# ------------------------------------------------------------ module trees
class Module:
    """One module of one target: its path, source span and whether it is test code."""

    def __init__(self, crate, target, path, src, idx, test):
        self.crate, self.target, self.path, self.src, self.idx, self.test = crate, target, path, src, idx, test
        self.span = src.inline[idx][:2] if idx is not None else (0, len(src.masked))
        fns = [f for f in src.fns if f[4] == idx]
        # Module-level items only: a fn or const nested in a fn body belongs to that fn.
        nested = lambda x: any(f[0] < x[0] and x[1] <= f[1] for f in fns)  # noqa: E731
        self.fns = [f for f in fns if not nested(f)]
        self.items = [i for i in src.items if i[4] == idx and not nested(i)]
        self.uses = [u for u in src.uses if u[4] == idx]
        self.seg = path[-1] if path else ""


def inert_assets(root):
    """Absolute paths of the assets: inert instruction files, and the manifests naming them.

    detect-docs-only.sh owns which files are inert; this only lists the
    candidates. A manifest is any other candidate whose text names an inert
    file by its path from the manifest's directory, as the PM instruction
    package's `file` bodies do: composing it reads those files.
    """
    cands = []
    for top in [os.path.join(root, "content"), *sorted(glob.glob(os.path.join(root, "crates", "*", "src", "assets")))]:
        for d, _, files in os.walk(top):
            cands += [os.path.relpath(os.path.join(d, f), root).replace(os.sep, "/") for f in files]
    proc = subprocess.run(["bash", DETECT, "--instruction-assets"], input="".join(c + "\n" for c in cands),
                          text=True, capture_output=True)
    if proc.returncode != 0:
        sys.exit(f"check_asset_test_filter: {DETECT} --instruction-assets exited {proc.returncode}: {proc.stderr}")
    inert = [os.path.join(root, p) for p in proc.stdout.splitlines() if p]
    manifests = []
    for c in sorted(set(cands) - set(proc.stdout.splitlines())):
        path = os.path.join(root, c)
        with open(path, encoding="utf-8", errors="replace") as f:
            text = f.read()
        here = os.path.dirname(path)
        for a in inert:
            rel = os.path.relpath(a, here).replace(os.sep, "/")
            if not rel.startswith("..") and re.search(r"(?<![\w./-])" + re.escape(rel) + r"(?![\w.-])", text):
                manifests.append(path)
                break
    return tuple(sorted(inert + manifests))


def crate_targets(crate_dir):
    """(crate, target-id, root file, is_test_target) for each lib, bin and test target cargo tests."""
    with open(os.path.join(crate_dir, "Cargo.toml"), "rb") as f:
        manifest = tomllib.load(f)
    package = manifest.get("package", {})
    pkg = package.get("name")
    if not pkg:
        return []
    out, names = [], set()

    def add(kind, name, rel, spec):
        full = os.path.normpath(os.path.join(crate_dir, rel))
        tid = "lib" if kind == "lib" else f"{kind}:{name}"
        if tid in names or not os.path.isfile(full):
            return
        names.add(tid)
        if spec.get("test", True):
            out.append((pkg, tid, full, kind == "test"))

    lib = manifest.get("lib", {})
    add("lib", "", lib.get("path", "src/lib.rs"), lib)
    for b in manifest.get("bin", []):
        name = b.get("name", pkg)
        add("bin", name, b.get("path") or ("src/main.rs" if name == pkg else f"src/bin/{name}.rs"), b)
    if package.get("autobins", True):
        add("bin", pkg, "src/main.rs", {})
        bindir = os.path.join(crate_dir, "src", "bin")
        for entry in sorted(os.listdir(bindir)) if os.path.isdir(bindir) else []:
            rel = f"src/bin/{entry}" if entry.endswith(".rs") else f"src/bin/{entry}/main.rs"
            add("bin", entry.removesuffix(".rs"), rel, {})
    for t in manifest.get("test", []):
        add("test", t["name"], t.get("path", f"tests/{t['name']}.rs"), t)
    if package.get("autotests", True):
        tdir = os.path.join(crate_dir, "tests")
        for entry in sorted(os.listdir(tdir)) if os.path.isdir(tdir) else []:
            rel = f"tests/{entry}" if entry.endswith(".rs") else f"tests/{entry}/main.rs"
            add("test", entry.removesuffix(".rs"), rel, {})
    return out


class Tree:
    """Every module of every tested target under <root>/crates/*."""

    def __init__(self, root):
        self.root = root
        self.assets = inert_assets(root)
        self.sources = {}
        self.modules = []
        self.by_path = {}
        self.targets = set()
        cdir = os.path.join(root, "crates")
        for name in sorted(os.listdir(cdir)):
            crate_dir = os.path.join(cdir, name)
            if os.path.isfile(os.path.join(crate_dir, "Cargo.toml")):
                for crate, tid, path, is_test in crate_targets(crate_dir):
                    self.targets.add((crate, tid))
                    self.walk(crate_dir, crate, tid, path, [], is_test, True, frozenset())

    def add(self, mod):
        self.modules.append(mod)
        self.by_path[(mod.crate, mod.target, tuple(mod.path))] = mod

    def walk(self, crate_dir, crate, tid, path, modpath, test, modrs, seen):
        if path in seen:
            return
        seen = seen | {path}
        if path not in self.sources:
            self.sources[path] = Source(path, crate_dir, self.assets)
        src = self.sources[path]
        self.add(Module(crate, tid, modpath, src, None, test))
        for idx, (_, _, _, attrs) in enumerate(src.inline):
            chain = src.chain(idx)
            names = [src.inline[c][2] for c in chain]
            is_test = test or any(CFG_TEST.search(src.inline[c][3]) for c in chain)
            self.add(Module(crate, tid, modpath + names, src, idx, is_test))
        fdir = os.path.dirname(path)
        stem = os.path.splitext(os.path.basename(path))[0]
        for _, name, attrs, owner in src.decls:
            chain = src.chain(owner)
            inline_dirs = [src.inline[c][2] for c in chain]
            is_test = test or bool(CFG_TEST.search(attrs)) or any(CFG_TEST.search(src.inline[c][3]) for c in chain)
            # Rust reference, "Module source filenames" and "The path attribute".
            base = fdir if modrs else os.path.join(fdir, stem)
            pm = re.search(r'#\[\s*path\s*=\s*"([^"]+)"', attrs)
            if pm:
                pbase = os.path.join(base, *inline_dirs) if chain else fdir
                cand = [os.path.normpath(os.path.join(pbase, pm.group(1)))]
            else:
                d = os.path.join(base, *inline_dirs)
                cand = [os.path.join(d, name + ".rs"), os.path.join(d, name, "mod.rs")]
            for c in cand:
                if os.path.isfile(c):
                    # A file named by #[path] resolves its own children like a mod.rs.
                    child_modrs = bool(pm) or os.path.basename(c) == "mod.rs"
                    self.walk(crate_dir, crate, tid, c, modpath + inline_dirs + [name], is_test, child_modrs, seen)
                    break

    def parent(self, mod):
        return self.by_path.get((mod.crate, mod.target, tuple(mod.path[:-1]))) if mod.path else None


# ------------------------------------------------------------ asset symbols
class Symbols:
    """Asset symbols by name, each with the module segments that home it."""

    def __init__(self, tree):
        self.tree = tree
        self.homes = {}  # name -> set(last module segment)
        self.kinds = {}  # name -> "const" | "loader"
        self._scope = {}
        self.test_loaders = {}  # R4 test-code helper fn name -> {(module segment, file, module start)}
        for home, name in CONTENT_LOADERS:
            self.homes.setdefault(name, set()).add(home)
            self.kinds.setdefault(name, "loader")
        asset_files = set()
        prod = [m for m in tree.modules if not m.test]
        for m in prod:
            if any(m.src.refs(s, e)[0] for s, e, *_ in m.items + m.fns):
                asset_files.add(m.src.path)
        changed = True
        while changed:
            changed = False
            for m in prod:
                for spans, kind in ((m.items, "const"), (m.fns, "loader")):
                    for s, e, name, attrs, _ in spans:
                        if CFG_TEST.search(attrs) or TEST_ATTR.search(attrs) or m.seg in self.homes.get(name, ()):
                            continue
                        if kind == "loader" and m.src.path not in asset_files:
                            continue
                        if self.reads(m, s, e):
                            self.homes.setdefault(name, set()).add(m.seg)
                            self.kinds.setdefault(name, kind)
                            if kind == "const":
                                asset_files.add(m.src.path)
                            changed = True
                for _, is_pub, segs, _, _ in m.uses:
                    if not is_pub or len(segs) < 2:
                        continue
                    home = self.resolve(m, segs[-2])
                    names = list(self.homes) if segs[-1] == "*" else [segs[-1]]
                    for name in names:
                        if home in self.homes.get(name, ()) and m.seg not in self.homes[name]:
                            self.homes[name].add(m.seg)
                            changed = True
        # R4: a non-test helper fn in test code that reads is a loader too. It is
        # keyed by its module (segment, file, span), never by segment alone:
        # dozens of test modules are all named `tests`, and a bare `d()` in one
        # must not match another's.
        changed = True
        while changed:
            changed = False
            for m in tree.modules:
                for s, e, name, attrs, _ in m.fns:
                    if TEST_ATTR.search(attrs) or not (m.test or CFG_TEST.search(attrs)):
                        continue
                    key = (m.seg, m.src.path, m.span[0])
                    if key in self.test_loaders.get(name, ()) or not self.reads(m, s, e):
                        continue
                    self.test_loaders.setdefault(name, set()).add(key)
                    changed = True

    def is_test_loader(self, m, name):
        return (m.seg, m.src.path, m.span[0]) in self.test_loaders.get(name, ())

    @staticmethod
    def resolve(m, seg):
        if seg == "super":
            return m.path[-2] if len(m.path) >= 2 else ""
        if seg == "self":
            return m.seg
        return seg

    def scope(self, m):
        """(names imported as (home, name), glob-imported homes, aliases) for module `m`."""
        key = id(m)
        if key in self._scope:
            return self._scope[key]
        names, globs, aliases = set(), {m.seg}, {}
        self._scope[key] = (names, globs, aliases)
        for _, _, segs, alias, _ in m.uses:
            if alias:
                aliases[alias] = self.resolve(m, segs[-1])
            if len(segs) < 2:
                continue
            if segs[-1] == "*":
                globs.add(self.resolve(m, segs[-2]))
                parent = self.tree.parent(m) if segs == ["super", "*"] else None
                if parent is not None:
                    pn, pg, pa = self.scope(parent)
                    names |= pn
                    globs |= pg
                    aliases.update(pa)
            else:
                names.add((self.resolve(m, segs[-2]), segs[-1]))
        return self._scope[key]

    def reads(self, m, s, e):
        """Why span [s, e) of module `m` reads an asset, or None."""
        lit, quals, bare = m.src.refs(s, e)
        if lit:
            return lit
        names, globs, aliases = self.scope(m)
        for seg, name in quals:
            home = aliases.get(seg, self.resolve(m, seg))
            if home in self.homes.get(name, ()):
                return f"{seg}::{name}"
        for name in bare & self.homes.keys():
            homes = self.homes[name]
            if homes & globs or any((h, name) in names for h in homes):
                return name
        # R4: a test-code loader, called bare from its own file or through an
        # import, or qualified by its own (non-`tests`) module segment.
        for seg, name in quals:
            keys = self.test_loaders.get(name, ())
            home = aliases.get(seg, self.resolve(m, seg))
            if home != "tests" and any(home == ks for ks, _, _ in keys):
                return f"{seg}::{name}"
        for name in bare & self.test_loaders.keys():
            for ks, path, start in self.test_loaders[name]:
                own = path == m.src.path and start == m.span[0]
                # `globs` always holds the module's own segment; only a real glob
                # import (`use super::*`) reaches a sibling module's helper.
                if own or (ks, name) in names or (ks in globs and ks != m.seg and path == m.src.path):
                    return name
        return None


# ----------------------------------------------------------------- readers
def find_readers(tree, symbols):
    """(crate, target, test path, file, line, reason) for every asset read in test code."""
    readers = set()
    for m in tree.modules:
        spans = []
        for s, e, name, attrs, _ in m.fns + m.items:
            is_test_fn = bool(TEST_ATTR.search(attrs))
            if m.test or is_test_fn or CFG_TEST.search(attrs):
                if not is_test_fn and symbols.is_test_loader(m, name):
                    # R4: its callers are the readers; the helper needs no row.
                    spans.append((s, e, None, True))
                    continue
                spans.append((s, e, name if is_test_fn else None, False))
        if m.test:
            edges = sorted((s, e) for s, e, _, _ in spans)
            lo, hi = m.span
            gaps, cur = [], lo
            for s, e in edges:
                if s > cur:
                    gaps.append((cur, s, None))
                cur = max(cur, e)
            if cur < hi:
                gaps.append((cur, hi, None))
            spans += [(s, e, t, False) for s, e, t in gaps]
        for s, e, test_fn, helper in spans:
            if helper:
                continue
            reason = symbols.reads(m, s, e)
            if reason:
                path = "::".join(m.path + ([test_fn] if test_fn else []))
                scope = "test" if test_fn else "module"
                readers.add((m.crate, m.target, path, os.path.relpath(m.src.path, tree.root), m.src.line(s), reason, scope))
    return sorted(readers)


# -------------------------------------------------------------------- list
def load_list(path):
    rows, errors = [], []
    with open(path, encoding="utf-8") as f:
        for n, raw in enumerate(f, 1):
            line = raw.rstrip("\n")
            if not line.strip() or line.lstrip().startswith("#"):
                continue
            cols = line.split("\t")
            if len(cols) not in (4, 5) or not all(c.strip() for c in cols):
                errors.append(f"{path}:{n}: expected 4 tab-separated columns (crate, target, filter, reason[, features])")
                continue
            crate, target, filt = (c.strip() for c in cols[:3])
            if not re.fullmatch(r"lib|(bin|test):[\w-]+", target):
                errors.append(f"{path}:{n}: target must be lib, bin:<name> or test:<name>, got {target!r}")
                continue
            if filt != "*" and not re.fullmatch(IDENT + r"(::" + IDENT + r")*", filt):
                errors.append(f"{path}:{n}: filter must be `*` or a test path, got {filt!r}")
                continue
            rows.append((crate, target, filt, n, cols[4].strip() if len(cols) == 5 else ""))
    return rows, errors


def covers(filt, hit):
    return filt == "*" or hit == filt or hit.startswith(filt + "::")


def cmd_check(args):
    rows, errors = load_list(args.list)
    tree = Tree(args.root)
    symbols = Symbols(tree)
    for crate, target, _, n, _ in rows:
        if (crate, target) not in tree.targets:
            errors.append(f"{args.list}:{n}: no tested target {target} in crate {crate}")
    if args.verbose:
        for name in sorted(symbols.homes):
            print(f"  {symbols.kinds[name]:6} {name}  homes={','.join(sorted(symbols.homes[name]))}")
    readers = find_readers(tree, symbols)
    uncovered = [r for r in readers if not any(c == r[0] and t == r[1] and covers(f, r[2]) for c, t, f, _, _ in rows)]
    if args.verbose:
        for r in readers:
            print(f"  read   {r[6]:6} {r[0]} {r[1]} {r[2] or '(crate root)'}  {r[3]}:{r[4]}  ({r[5]})")
    loaders = sum(1 for k in symbols.kinds.values() if k == "loader")
    print(f"check_asset_test_filter: {len(symbols.homes)} asset symbols ({loaders} loaders), "
          f"{len(readers)} asset reads in test code, {len(rows)} list rows")
    for r in uncovered:
        print(f"::error file={r[3]},line={r[4]}::asset-reading test outside {os.path.basename(args.list)}: "
              f"{r[0]} {r[1]} {r[2] or '(crate root)'} reads {r[5]}")
    for e in errors:
        print(f"::error::{e}")
    if uncovered or errors:
        print(f"check_asset_test_filter: FAIL — {len(uncovered)} uncovered read(s), {len(errors)} list error(s). "
              f"Add a row naming the test or its module to {args.list}.")
        return 1
    print("check_asset_test_filter: every asset read in test code is covered by the list")
    return 0


def plan(rows):
    groups = {}
    feats = {(c, t): ["--features", ft] for c, t, _, _, ft in rows if ft}  # 5th column: `-p` build features (#8378)
    for crate, target, filt, _, _ in rows:
        groups.setdefault((crate, target), []).append(filt)
    out = []
    for (crate, target), filters in groups.items():
        kind, _, name = target.partition(":")
        sel = ["--lib"] if kind == "lib" else [f"--{kind}", name]
        out.append(((crate, target), filters, ["cargo", "test", "-p", crate, *sel, *feats.get((crate, target), []), "--locked", "--no-fail-fast", "--", *([] if "*" in filters else sorted(set(filters)))]))
    return out


def cmd_plan(args):
    rows, errors = load_list(args.list)
    for e in errors:
        print(f"::error::{e}")
    for _, _, cmd in plan(rows):
        print(" ".join(cmd))
    return 1 if errors else 0


def cmd_run(args):
    rows, errors = load_list(args.list)
    if errors or not rows:
        for e in errors or [f"{args.list} lists no test"]:
            print(f"::error::{e}")
        return 1
    status, total, t0 = 0, 0, time.monotonic()
    for (crate, target), filters, cmd in plan(rows):
        print(f"::group::{' '.join(cmd)}", flush=True)
        t = time.monotonic()
        proc = subprocess.run(cmd, cwd=args.root, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        print(proc.stdout, end="")
        print("::endgroup::")
        ran = re.findall(r"^test (\S+) \.\.\. (ok|FAILED|ignored)", proc.stdout, re.M)
        total += len(ran)
        print(f"asset-content tests: {crate} {target}: {len(ran)} tests, {time.monotonic() - t:.0f}s, exit {proc.returncode}",
              flush=True)
        if proc.returncode != 0:
            status = 1
            print(f"::error::{crate} {target}: cargo test exited {proc.returncode}")
        for filt in sorted(set(filters)):
            if not any(filt == "*" or filt in name for name, _ in ran):
                status = 1
                print(f"::error::{crate} {target}: filter {filt!r} selected no test — renamed or deleted? (#8378)")
    print(f"asset-content tests: {total} tests in {time.monotonic() - t0:.0f}s, exit {status}")
    return status


def main():
    here = os.path.dirname(os.path.abspath(__file__))
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("command", choices=["check", "plan", "run"])
    ap.add_argument("--root", default=os.path.dirname(here))
    ap.add_argument("--list", default=None)
    ap.add_argument("--verbose", action="store_true")
    args = ap.parse_args()
    args.root = os.path.abspath(args.root)
    args.list = args.list or os.path.join(args.root, "scripts", "asset-content-tests.tsv")
    if not os.path.isfile(args.list) or not os.path.isdir(os.path.join(args.root, "crates")):
        print(f"check_asset_test_filter: missing {args.list} or {args.root}/crates", file=sys.stderr)
        return 2
    return {"check": cmd_check, "plan": cmd_plan, "run": cmd_run}[args.command](args)


if __name__ == "__main__":
    sys.exit(main())
