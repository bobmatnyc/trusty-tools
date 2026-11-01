"""dashboard_projects.py: the Projects nav's data (GET /api/projects), read-only.

Why: the owner 2026-10-27 13:3xZ: a "Projects" nav option with project IDs, root paths, links to the sessions running in
the project, stack info, configuration and custom agent info. Prototype in Python; it moves to Rust for 2.0
(a Rust crate served by the tm daemon), so the data model below is the contract and this module stays small.
What: build() joins the tm registry, the server's doc allowlist, live tmux panes and the project files into one list:
  project   {id, label, no_github_url, registered_path, roots, root, owner, repo}  the list = exactly the projects that
            `tm project list` prints: the records of tm's project-registry/projects.json (registered_projects); a path-only
            entry (project-paths.json, doc allowlist) never makes a row. id = the tm alias; label = `owner/project` from the
            registered GitHub URL, else the alias with no_github_url=true (an owner is never guessed); rows sort by label.
            registered_path = the alias's row in project-paths.json when it has one. The doc allowlist (load_projects) only
            ADDS roots (`roots`) to a registered row, where the tmux sessions actually run. `root` = the directory scanned:
            the first extra root, else the registered path;
  sessions  [{name, id, attached, activity, panes:[{window, pane, path, command, activity}]}]  live tmux sessions
            with a pane inside any root; id = the dashboard session row id (S07), resolved from the sessions collection;
  stack     [{kind, name, version}]  from manifests at the root (Cargo.toml, package.json, pyproject.toml, go.mod, ...);
  config    [{file, kind, bytes, fields, doc}]  which trusty-mpm / Claude files exist, with non-secret fields only;
  agents    [{file, name, description, model}]  from .claude/agents/*.md frontmatter.
  archived  [{id, label, owner, repo, state, archived_root, original_root, at}]  projects archived or decommissioned by
            scripts/project_lifecycle.py (local/project-lifecycle.json); they leave `projects` and so the nav (ar-Tb3).
Safety: read-only; every file read is capped at READ_CAP bytes; ~/Documents, ~/Desktop and iCloud paths are never
touched; a file named like a secret store is skipped; env values, tokens and other values are never returned, only
names; a symlink leaving the root is ignored; results are cached (scan 120 s per root, whole answer 10 s).
Test: scripts/test_dashboard_projects.py
"""
import json
import os
import re
import subprocess
import threading
import time

try:
    import tomllib
except ImportError:   # pragma: no cover - Python < 3.11
    tomllib = None

READ_CAP = 65536
MAX_AGENTS = 100
MAX_FIELDS = 24
SCAN_TTL_S = 120.0
ANSWER_TTL_S = 10.0
SECRET_NAME = re.compile(r"secret|credential|token|password|passwd|apikey|api[_-]?key|private|\.env|\.pem$|\.key$|id_rsa", re.I)
LONG_TOKEN = re.compile(r"[A-Za-z0-9_\-+/=]{32,}")
FORBIDDEN_DIRS = ("Documents", "Desktop", "Library/Mobile Documents")
CONFIG_FILES = (   # (relative path, kind)
    (".trusty-mpm.toml", "toml"), (".trusty-mpm/config.toml", "toml"), (".claude/settings.json", "settings"),
    (".claude/settings.local.json", "settings"), (".mcp.json", "mcp"), ("CLAUDE.md", "md"), ("AGENTS.md", "md"),
    ("TICKETING.md", "md"),
)


def forbidden(path, home=None):
    """True for ~/Documents, ~/Desktop and iCloud Drive paths: never stat or read them (the host freezes)."""
    home = home or os.path.expanduser("~")
    p = os.path.abspath(path)
    return any(p == os.path.join(home, d) or p.startswith(os.path.join(home, d) + os.sep) for d in FORBIDDEN_DIRS)


def secret_like(name):
    return bool(SECRET_NAME.search(name))


def _inside(root, real):
    return real == root or real.startswith(root.rstrip(os.sep) + os.sep)


def read_capped(root, rel, cap=READ_CAP):
    """Text of root/rel (first `cap` bytes) or None: absent, not a regular file, a secret-like name, a forbidden
    path, or a symlink that resolves outside root."""
    if secret_like(os.path.basename(rel)):
        return None
    path = os.path.join(root, rel)
    if forbidden(path):
        return None
    try:
        real = os.path.realpath(path)
        if not _inside(os.path.realpath(root), real) or not os.path.isfile(real):
            return None
        with open(real, "rb") as f:
            return f.read(cap).decode("utf-8", "replace")
    except OSError:
        return None


def _toml(text):
    if tomllib is None or text is None:
        return {}
    try:
        d = tomllib.loads(text)
    except (ValueError, tomllib.TOMLDecodeError):
        return {}
    return d if isinstance(d, dict) else {}


def _json(text):
    try:
        d = json.loads(text) if text else {}
    except ValueError:
        return {}
    return d if isinstance(d, dict) else {}


# ---------------------------------------------------------------- stack

def stack_info(root):
    """[{kind, name, version}] from manifests at the root; names and versions only (None when a manifest carries none)."""
    out = []

    def add(kind, name=None, version=None):
        out.append({"kind": kind, "name": name if isinstance(name, str) else None, "version": version if isinstance(version, (str, int, float)) and not isinstance(version, bool) else None})

    t = read_capped(root, "Cargo.toml")
    if t is not None:
        d = _toml(t)
        pkg, ws = d.get("package") or {}, d.get("workspace") or {}
        ver = pkg.get("version")
        if isinstance(ver, dict):   # version.workspace = true
            ver = (ws.get("package") or {}).get("version")
        add("rust", pkg.get("name") or ("workspace (%d members)" % len(ws.get("members") or []) if ws else None), ver or (ws.get("package") or {}).get("version"))
    t = read_capped(root, "package.json")
    if t is not None:
        d = _json(t)
        add("node", d.get("name"), d.get("version"))
    t = read_capped(root, "pyproject.toml")
    if t is not None:
        d = _toml(t)
        proj = d.get("project") or (d.get("tool") or {}).get("poetry") or {}
        add("python", proj.get("name"), proj.get("version") or proj.get("requires-python"))
    t = read_capped(root, "go.mod")
    if t is not None:
        mod = re.search(r"^module\s+(\S+)", t, re.M)
        go = re.search(r"^go\s+(\S+)", t, re.M)
        add("go", mod.group(1) if mod else None, go.group(1) if go else None)
    for fn, kind in (("Gemfile", "ruby"), ("pom.xml", "java"), ("build.gradle", "java"), ("build.gradle.kts", "java"),
                     ("composer.json", "php"), ("mix.exs", "elixir"), ("deno.json", "deno"), ("requirements.txt", "python"), ("Package.swift", "swift")):
        if kind == "python" and any(s["kind"] == "python" for s in out):
            continue
        if kind == "java" and any(s["kind"] == "java" for s in out):
            continue
        try:
            if os.path.isfile(os.path.join(root, fn)) and not forbidden(os.path.join(root, fn)):
                add(kind, fn)
        except OSError:
            pass
    return out


# ---------------------------------------------------------------- configuration

def _scalar_fields(d, prefix=""):
    """Flat {dotted key: scalar} from a parsed toml/json dict, two levels deep; secret-named keys and long values are dropped."""
    out = {}
    for k, v in d.items():
        if not isinstance(k, str) or secret_like(k):
            continue
        key = prefix + k
        if isinstance(v, dict) and not prefix:
            out.update(_scalar_fields(v, key + "."))
        elif isinstance(v, bool) or isinstance(v, (int, float)):
            out[key] = v
        elif isinstance(v, str) and len(v) <= 80 and not LONG_TOKEN.search(v):
            out[key] = v
        elif isinstance(v, list) and not prefix and len(v) <= 8 and all(isinstance(x, str) and len(x) <= 40 and not LONG_TOKEN.search(x) for x in v):
            out[key] = ", ".join(v)
    return dict(list(out.items())[:MAX_FIELDS])


def _settings_fields(d):
    """settings.json: structure only. Env var NAMES (never values), hook event names, plugin counts, permission mode, model."""
    out = {}
    if isinstance(d.get("model"), str):
        out["model"] = d["model"]
    perm = d.get("permissions")
    if isinstance(perm, dict) and isinstance(perm.get("defaultMode"), str):
        out["permissions.defaultMode"] = perm["defaultMode"]
    env = d.get("env")
    if isinstance(env, dict) and env:
        out["env (names only)"] = ", ".join(sorted(str(k) for k in env)[:MAX_FIELDS])
    hooks = d.get("hooks")
    if isinstance(hooks, dict) and hooks:
        out["hooks"] = ", ".join(sorted(str(k) for k in hooks))
    plugins = d.get("enabledPlugins")
    if isinstance(plugins, dict) and plugins:
        out["plugins"] = "%d enabled, %d disabled" % (sum(1 for v in plugins.values() if v), sum(1 for v in plugins.values() if not v))
    return out


def _mcp_fields(d):
    servers = d.get("mcpServers")
    return {"mcp servers": ", ".join(sorted(str(k) for k in servers)[:MAX_FIELDS])} if isinstance(servers, dict) and servers else {}


def config_info(root, doc_name=None):
    """[{file, kind, bytes, fields, doc}] for each known config file that exists, plus the .trusty-mpm/ directory listing.
    doc = the #doc/<project>/<rel> link when the reader can serve it (a root-level .md under a doc-allowlist project)."""
    out = []
    for rel, kind in CONFIG_FILES:
        text = read_capped(root, rel)
        if text is None:
            continue
        try:
            size = os.path.getsize(os.path.join(root, rel))
        except OSError:
            size = len(text)
        fields = {}
        if kind == "toml":
            fields = _scalar_fields(_toml(text))
        elif kind == "settings":
            fields = _settings_fields(_json(text))
        elif kind == "mcp":
            fields = _mcp_fields(_json(text))
        out.append({"file": rel, "kind": kind, "bytes": size, "fields": fields,
                    "doc": "#doc/%s/%s" % (doc_name, rel) if kind == "md" and doc_name and "/" not in rel else None})
    d = os.path.join(root, ".trusty-mpm")
    try:
        if not forbidden(d) and os.path.isdir(d) and _inside(os.path.realpath(root), os.path.realpath(d)):
            names = sorted(n for n in os.listdir(d) if not secret_like(n))[:MAX_FIELDS]
            out.append({"file": ".trusty-mpm/", "kind": "dir", "bytes": None, "fields": {"entries": ", ".join(names)} if names else {}, "doc": None})
    except OSError:
        pass
    return out


# ---------------------------------------------------------------- agents

def frontmatter(text):
    """{key: value} for single-line `key: value` pairs between the leading --- fences (values unquoted); {} when none."""
    if not text or not text.startswith("---"):
        return {}
    end = text.find("\n---", 3)
    if end < 0:
        return {}
    out, key = {}, None
    for line in text[3:end].splitlines():
        m = re.match(r"^([A-Za-z_][\w-]*):\s*(.*)$", line)
        if m:
            key, v = m.group(1), m.group(2).strip()
            if len(v) >= 2 and v[0] == v[-1] and v[0] in "\"'":
                v = v[1:-1]
            out[key] = "" if v in (">", "|", ">-", "|-") else v   # block scalar: the indented lines below fill it
        elif key and line[:1] in (" ", "\t") and line.strip() and len(out[key]) < 400:
            out[key] = (out[key] + " " + line.strip()).strip()
    return out


def agents_info(root):
    """[{file, name, description, model}] for .claude/agents/*.md (first 4 KB each, frontmatter only), sorted by file."""
    d = os.path.join(root, ".claude", "agents")
    try:
        if forbidden(d) or not os.path.isdir(d):
            return []
        names = sorted(n for n in os.listdir(d) if n.endswith(".md"))[:MAX_AGENTS]
    except OSError:
        return []
    out = []
    for n in names:
        fm = frontmatter(read_capped(root, ".claude/agents/" + n, 4096))
        desc = fm.get("description", "")
        out.append({"file": ".claude/agents/" + n, "name": fm.get("name") or n[:-3],
                    "description": desc if len(desc) <= 240 else desc[:237] + "...", "description_full": desc[:1000], "model": fm.get("model") or None})
    return out


# ---------------------------------------------------------------- sessions and assembly

TMUX_FMT = "#{session_name}\t#{window_id}\t#{pane_id}\t#{pane_current_path}\t#{pane_current_command}\t#{session_attached}\t#{window_activity}"


def _iso(epoch):
    try:
        return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(int(epoch)))
    except (ValueError, OverflowError, OSError):
        return None


def tmux_panes(run=subprocess.run):
    """{tmux session: {"attached": bool, "panes": [{window, pane, path, command, activity}]}} from `tmux list-panes -a`;
    {} when tmux is absent or has no server. activity = the pane's window last-activity time, ISO UTC."""
    try:
        r = run(["tmux", "list-panes", "-a", "-F", TMUX_FMT], capture_output=True, text=True, timeout=3)
    except (OSError, subprocess.SubprocessError):
        return {}
    out = {}
    if r.returncode == 0:
        for line in r.stdout.splitlines():
            f = line.split("\t")
            if len(f) != 7 or not f[0] or not f[3]:
                continue
            e = out.setdefault(f[0], {"attached": False, "panes": []})
            e["attached"] = e["attached"] or f[5] not in ("0", "")
            e["panes"].append({"window": f[1], "pane": f[2], "path": f[3], "command": f[4], "activity": _iso(f[6])})
    return out


def project_rows(registry, allow):
    """One row per tm-registered project: {id, label, no_github_url, registered_path, roots, root, owner, repo, doc}.
    registry = [{name, path, owner, repo}] (registered_projects; path None when project-paths.json has no row); allow =
    {name: root} from the doc allowlist, matched by case-insensitive name: it only ADDS a root (and the reader's doc name) to a
    registered project; an allowlist name the registry lacks gets no row (the owner 2026-10-27: only trusty-mpm projects).
    label = owner/repo from the registered GitHub URL, else the alias (no_github_url); sorted by label."""
    rows, by = [], {}
    for r in registry:
        owner, repo = r.get("owner"), r.get("repo")
        has_url = bool(owner and repo)
        row = {"id": r["name"], "label": "%s/%s" % (owner, repo) if has_url else r["name"], "no_github_url": not has_url,
               "registered_path": r.get("path"), "roots": [], "root": r.get("path"), "owner": owner, "repo": repo, "doc": None,
               "note": r.get("note")}
        rows.append(row)
        by[r["name"].lower()] = row
    for name, root in allow.items():
        row = by.get(name.lower())
        if row is None:
            continue
        row["doc"] = name
        if not row["registered_path"] or os.path.realpath(row["registered_path"]) != os.path.realpath(root):
            row["roots"].append(root)
            row["root"] = row["roots"][0]
    return sorted(rows, key=lambda r: (r["label"].lower(), r["id"].lower()))


def sessions_in(roots, panes, sess_ids):
    """[{name, id, attached, activity, panes}] for tmux sessions with a pane whose real path is inside any of `roots`; only
    the panes inside are listed (at most 8 per session); sorted by name. activity = newest pane activity."""
    real_roots = [os.path.realpath(r) for r in roots if r and not forbidden(r)]
    hit = []
    for s, info in panes.items():
        inside = []
        for pn in info.get("panes", []):
            if forbidden(pn["path"]):
                continue
            rp = os.path.realpath(pn["path"])
            if any(_inside(r, rp) for r in real_roots):
                inside.append(pn)
        if inside:
            acts = [x["activity"] for x in inside if x.get("activity")]
            hit.append({"name": s, "id": sess_ids.get(s), "attached": bool(info.get("attached")), "activity": max(acts) if acts else None, "panes": inside[:8]})
    return sorted(hit, key=lambda x: x["name"])


class Scans:
    """Per-root memo of the disk scan (stack, config, agents) for SCAN_TTL_S, so a page view never walks the disk twice."""

    def __init__(self, ttl=SCAN_TTL_S, clock=time.time):
        self.ttl, self.clock, self._m, self._lock = ttl, clock, {}, threading.Lock()

    def get(self, root, doc):
        with self._lock:
            hit = self._m.get((root, doc))
            if hit and self.clock() - hit[0] < self.ttl:
                return hit[1]
        val = {"stack": stack_info(root), "config": config_info(root, doc), "agents": agents_info(root)}
        with self._lock:
            self._m[(root, doc)] = (self.clock(), val)
        return val


LIFECYCLE_HIDDEN = ("archived", "decommissioned")


def build(registry, allow, panes, sess_ids, scans=None, lifecycle=None):
    """{"projects": [...], "archived": [...]}: one entry per tm-registered project that is not archived or decommissioned, sorted by
    label (owner/project). A root that is forbidden (iCloud) or missing is listed with scanned=false and nothing read.
    lifecycle = {alias: {state, label, ...}} from local/project-lifecycle.json (scripts/project_lifecycle.py; tm has no archive verb):
    those aliases leave the nav and appear in "archived" (the owner 2026-10-27, ar-Tb3)."""
    scans = scans or Scans()
    lifecycle = lifecycle or {}
    registry = [r for r in registry if (lifecycle.get(r["name"]) or {}).get("state") not in LIFECYCLE_HIDDEN]
    out = []
    for row in project_rows(registry, allow):
        root = row["root"]
        # #10571: one resolved root (tm's lowercase clone rule, then realpath) for both the existence check and sessions_in
        ok = bool(root) and not forbidden(root) and os.path.isdir(os.path.realpath(root))
        scan = scans.get(root, row["doc"]) if ok else {"stack": [], "config": [], "agents": []}
        all_roots = [x for x in [row["registered_path"]] + row["roots"] if x]
        out.append({"id": row["id"], "label": row["label"], "no_github_url": row["no_github_url"], "registered_path": row["registered_path"], "roots": row["roots"], "root": root,
                    "owner": row["owner"], "repo": row["repo"], "scanned": ok, "root_note": root_note(row, ok),
                    "sessions": sessions_in(all_roots, panes, sess_ids) if ok else [], **scan})
    arch = [{"id": a, "label": v.get("label") or a, "owner": v.get("owner"), "repo": v.get("repo"), "state": v.get("state"), "archived_root": v.get("archived_root"),
             "original_root": v.get("original_root"), "at": v.get("at")} for a, v in sorted(lifecycle.items(), key=lambda kv: (kv[1].get("label") or kv[0]).lower())
            if isinstance(v, dict) and v.get("state") in LIFECYCLE_HIDDEN]
    return {"projects": out, "archived": arch}


GITHUB_URL = re.compile(r"github\.com[/:]([A-Za-z0-9_.-]+)/([A-Za-z0-9_.-]+?)(?:\.git)?/?$")


class RegistryError(ValueError):
    """tm's project registry exists but is not in the shape tm writes."""


def mpm_root(env=None, home=None):
    """tm's root: $TRUSTY_MPM_ROOT when set and non-empty, else ~/.trusty-mpm."""
    env = os.environ if env is None else env
    return env.get("TRUSTY_MPM_ROOT") or os.path.join(home or os.path.expanduser("~"), ".trusty-mpm")


def _registry_json(path):
    """The parsed file, None when absent; RegistryError (path only, never content) when unreadable or not JSON."""
    try:
        with open(path, encoding="utf-8") as f:
            return json.load(f)
    except FileNotFoundError:
        return None
    except (OSError, ValueError):
        raise RegistryError(f"The project list could not be read, so the Projects view is empty. File: {path}") from None


def registered_projects(root=None):
    """The tm-registered projects (`tm project list`): [{name, path, owner, repo}] in file order.

    Why: #10571, the Projects view must work on any install, with no secrets module: listing projects reads no secret.
    What: one row per record of <root>/project-registry/projects.json, the file tm's ProjectStore writes
    ({"projects": {name: {name, repo_url, default_branch, ...}}}); name = the record key (the alias). owner and repo come
    from a github.com repo_url, None otherwise. path is the alias's row in <root>/project-paths.json ([{alias, path}],
    tm's ProjectAliasEntry; exact alias, else case-insensitive), kept when absolute, else None; that file never adds a
    project. When the map gives no path, the canonical clone <projects_root>/<owner>/<repo> is used (projects_root from
    local/architect.json, default ~/trusty-mpm-projects); `path_source` says which ("project map", "canonical layout",
    or None with no path). The path may not exist; build() says so. root defaults to mpm_root(). Absent files give [];
    a file in another shape is a RegistryError, which the view shows as registry_error.
    Test: tests/test_dashboard_projects_registry.py
    """
    root = root or mpm_root()
    projects_root = _projects_root()
    store = _registry_json(os.path.join(root, "project-registry", "projects.json")) or {}
    rows = _registry_json(os.path.join(root, "project-paths.json")) or []
    if not isinstance(rows, list) or not isinstance(store, dict):
        raise RegistryError("The project list is not in the format tm writes, so the Projects view is empty.")
    records = store.get("projects", {})
    if not isinstance(records, dict):   # #10571: never a silent empty list for a file in another shape
        raise RegistryError("The project list file has an unexpected shape, so the Projects view is empty.")
    paths, folded = {}, {}
    for r in rows:
        if isinstance(r, dict) and isinstance(r.get("alias"), str) and isinstance(r.get("path"), str) and os.path.isabs(r["path"]):
            paths.setdefault(r["alias"], r["path"])
            folded.setdefault(r["alias"].lower(), r["path"])
    out = []
    for name, rec in records.items():
        if not isinstance(rec, dict):   # #10571: say what is wrong with this one row, keep the others
            out.append({"name": name, "path": None, "path_source": None, "owner": None, "repo": None,
                        "note": "This project's entry in the project list is unreadable, so its folder is unknown."})
            continue
        m = GITHUB_URL.search(rec.get("repo_url") or "")
        owner, repo = (m.group(1), m.group(2)) if m else (None, None)
        path, source = paths.get(name) or folded.get(name.lower()), "project map"
        if not path and owner and repo and clone_slug(owner) and clone_slug(repo):
            path, source = os.path.join(projects_root, clone_slug(owner), clone_slug(repo)), "canonical layout"
        out.append({"name": name, "path": path, "path_source": source if path else None, "owner": owner, "repo": repo})
    return out


def clone_slug(segment):
    """One path segment exactly as tm names its clone directory (#10571).

    tm clones a repo to <repos_root>/<owner>/<repo> with trusty-mpm base_clone_path (src/core/repo_paths.rs:108),
    from a GithubPath that trusty-common parse_github_path (src/github_path.rs:177) builds with
    slugify_component (src/github_path.rs:89): ASCII-lowercase, strip a trailing ".git", keep
    [a-z0-9], collapse every other run to one "-", and trim "-" at both ends. So Example-Org-LLC/Widget_Shop.git
    is cloned at example-org-llc/widget-shop.
    """
    s = segment.strip().lower()
    if s.endswith(".git"):
        s = s[:-4]
    return re.sub(r"[^a-z0-9]+", "-", s).strip("-")


def _projects_root():
    """local/architect.json `projects_root` (ARCHITECT_PROJECTS_ROOT wins), else ~/trusty-mpm-projects (#10571)."""
    try:
        import architect_config
        return architect_config.load().projects_root
    except Exception:  # an unusable config keeps the default layout
        return os.path.expanduser("~/trusty-mpm-projects")


def root_note(row, ok):
    """"" when the root was read; else a plain-English line saying the project's folder was not found (#10571)."""
    if ok:
        return ""
    if row.get("note"):
        return row["note"]
    if not row.get("root"):
        return "Root not found: no folder is known for this project, so its files and sessions are not shown."
    if forbidden(row["root"]):
        return "Not read: this project's folder is in a private location the dashboard does not open."
    return "Root not found: the folder %s does not exist on this machine, so its files and sessions are not shown." % row["root"]


def _lifecycle_state():
    """local/project-lifecycle.json projects; {} while project_lifecycle is not shipped (#10571)."""
    try:
        import project_lifecycle
    except ImportError:
        return {}
    return project_lifecycle.load_state()["projects"]



class ProjectsView:
    """What the server holds: injectable sources, a 10 s answer cache over the 120 s scan memo. A registry that cannot be
    read is reported in registry_error (the page shows it) and the list is empty: the allowlist never adds rows."""

    def __init__(self, registry, allow, panes=tmux_panes, clock=time.time, lifecycle=None):
        self.registry, self.allow, self.panes, self.clock = registry, allow, panes, clock
        self.lifecycle = lifecycle or _lifecycle_state
        self.scans, self._at, self._val, self._lock = Scans(clock=clock), 0.0, None, threading.Lock()

    def invalidate(self):
        with self._lock:
            self._val = None

    def get(self, sess_ids):
        with self._lock:
            if self._val is None or self.clock() - self._at >= ANSWER_TTL_S:
                reg, err = [], None
                try:
                    reg = self.registry()
                except RegistryError as e:   # plain words for the page; the file path is the detail
                    err = str(e)
                except Exception as e:   # noqa: BLE001 - a malformed registry is reported on the page, never a silent empty list
                    err = "%s: %s" % (type(e).__name__, e)
                self._val = build(reg, self.allow(), self.panes(), sess_ids, self.scans, self.lifecycle())
                self._val["registry_error"] = err
                self._at = self.clock()
            return self._val
