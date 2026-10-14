#!/usr/bin/env python3
"""verify-claim.py - check the claims in one PM-message event (T07, D6b).

Why: each PM message ("head X ready for clearance", "#N merged as SHA",
  "published") cost the Architect a verify round of gh, git and crates.io
  calls. This script runs that round with no model and prints one line per
  claim. A claim stays "reported" until a MATCH line says otherwise.
What: reads one event JSON (a file path, "-" for stdin, or a literal JSON
  object), checks every claim it can, and prints one line per claim:
    MATCH <claim>
    MISMATCH <claim>: expected X, got Y
    UNCHECKED <claim>: <reason>
  A line whose value came from the REST fallback ends with " (via rest)".
  Exit 0 = no MISMATCH (UNCHECKED lines do not fail the run), 1 = any
  MISMATCH, 2 = bad input. Read-only: it runs only `gh pr view`,
  `gh pr checks`, `gh run view`, read-only git verbs and one crates.io GET,
  and every command passes a guard that refuses anything else. `gh api` is
  allowed only as a plain GET of repos/<o>/<r>/pulls|issues|commits/... (the
  REST fallback and the issue claims below).
Test: scripts/test_verify_claim.py

Event schema (unknown values are null, never guessed):
  {ts, session, kind, repo, pr, branch, head, base, merge_sha, crate,
   version, ci_run, claims, asks, text_sha}
  ts         UTC time the event was written, "YYYY-MM-DDTHH:MM:SSZ".
  session    tmux name of the PM that sent the message, e.g. "tm-dogfood".
  kind       clearance_request | merged | published | paused | status |
             improvement | question | fyi (required).
  repo       "owner/name" or a bare name from REPOS below.
  pr         PR number the message reports on, else null.
  branch     branch name the message names, else null.
  head       head commit the message reports on or asks clearance for.
  base       base commit the message names. For a PR it is compared with
             the PR's baseRefOid; for a local branch head must descend from it.
  merge_sha  merge commit of a merged PR.
  crate      crate name, for crates.io checks.
  version    version of the release or publish the message is about.
  ci_run     GitHub Actions run id.
  claims     extra checkable numbers and states, all optional:
               pr_state     OPEN | CLOSED | MERGED (kind merged implies MERGED)
               merged_at    ISO time; a prefix of the real value of >= 10
                            characters matches
               files        changed-file count (PR: changedFiles; no PR:
                            `git diff --numstat base head` lines)
               commits      commit count over base (PR: commits;
                            no PR: `git rev-list --count base..head`)
               checks       {"pass": n, "fail": n, "pending": n}, any subset,
                            from `gh pr checks --json bucket`
               crates_io_max  version crates.io lists as max_version now
                            (kind published implies version)
               ci_conclusion  expected conclusion of ci_run
               ci_head      expected headSha of ci_run
               issues       list of {n, state: open|closed, reason?}; reason is
                            the GitHub state_reason (completed, not_planned,
                            reopened). Checked with REST repos/<o>/<r>/issues/<n>.
  asks       one line: what the PM wants from the Architect.
  text_sha   sha1 of the message text, for de-duplication.
SHAs match by prefix and need at least 10 hex characters; a shorter claim is
UNCHECKED. With a PR number the PR claims run through gh; without one the
head/base/commits/files claims run against the local checkout in REPOS.
GraphQL rate limit: when `gh pr view` or `gh pr checks` fails with a rate-limit
error, the same fields are read over REST (pulls/<n>, commits/<sha>/check-runs);
if REST fails too the claim stays UNCHECKED, never a guessed value.

Environment: VERIFY_CLAIM_OFFLINE=1 makes every gh and curl check UNCHECKED (the
eval uses it so extractor runs do not hit GitHub).
"""

from __future__ import annotations

import json
import os
import re
import subprocess
import sys
from typing import Callable

HOME = os.path.expanduser("~")
# Local checkout root of the repos below (#8436: no operator path built in).
PROJECTS = os.environ.get("ARCHITECT_REPOS_DIR") or os.path.join(HOME, "projects")

# Bare repo name (lower case) -> (owner/name, local checkout).
REPOS: dict[str, tuple[str, str]] = {
    "trusty-tools": ("example-org/trusty-tools", f"{PROJECTS}/example-org/trusty-tools"),
    "webapp": ("employer-org/WEBAPP", f"{PROJECTS}/employer-org/webapp"),
    "side-project": ("example-org/side-project", f"{PROJECTS}/example-org/side-project"),
}
# GitHub owner (lower case) -> gh config dir; None keeps the inherited default
# config (GH_CONFIG_DIR as the caller's environment has it).
OWNER_CONFIG: dict[str, str | None] = {
    "employer-org": f"{HOME}/.config/gh-employer",
    "example-org": None,
    "consult-org": None,
    "travel-org": None,
}
KINDS = (
    "clearance_request", "merged", "published", "paused",
    "status", "improvement", "question", "fyi",
)
USER_AGENT = "trusty-architect"
REST_PATH = re.compile(r"repos/[\w.-]+/[\w.-]+/(pulls|issues|commits)/[\w./-]+(\?per_page=\d+)?")
RATE_LIMIT = re.compile(r"rate limit", re.I)
MIN_SHA = 10

Runner = Callable[[list, "dict | None"], tuple]  # (argv, env) -> (rc, stdout, stderr)


class BadInput(Exception):
    """The event is not a usable event object."""


def default_runner(argv: list, env: dict | None = None) -> tuple:
    """Run one command with a timeout; never raises on a missing binary."""
    try:
        p = subprocess.run(argv, capture_output=True, text=True, env=env, timeout=60)
    except (OSError, subprocess.TimeoutExpired) as e:
        return 127, "", f"{type(e).__name__}: {e}"
    return p.returncode, p.stdout, p.stderr


def _git_sub(argv: list) -> str:
    i = 1
    while i < len(argv) and argv[i] == "-C":
        i += 2
    return argv[i] if i < len(argv) else ""


def tool_is_network(argv: list) -> bool:
    return argv[0] in ("gh", "curl")


def guard_read_only(argv: list) -> None:
    """Refuse any command outside the read-only set this script may run."""
    tool = argv[0]
    if tool == "gh":
        ok = tuple(argv[1:3]) in {("pr", "view"), ("pr", "checks"), ("run", "view")} or (
            len(argv) == 3 and argv[1] == "api" and bool(REST_PATH.fullmatch(argv[2])))
    elif tool == "git":
        ok = _git_sub(argv) in {"rev-parse", "merge-base", "rev-list", "diff"}
    elif tool == "curl":
        bad = {"-X", "-d", "--data", "-T", "-o", "-O", "--request", "--upload-file"}
        ok = argv[-1].startswith("https://crates.io/api/v1/crates/") and not bad & set(argv)
    else:
        ok = False
    if not ok:
        raise PermissionError(f"verify-claim refuses non-read-only command: {argv[:3]}")


# ---------------------------------------------------------------- input


def parse_event(raw: object) -> dict:
    """Validate the event shape; returns a dict with every field present."""
    if not isinstance(raw, dict):
        raise BadInput("event must be a JSON object")
    ev = {k: raw.get(k) for k in (
        "ts", "session", "kind", "repo", "pr", "branch", "head", "base",
        "merge_sha", "crate", "version", "ci_run", "claims", "asks", "text_sha")}
    if ev["kind"] not in KINDS:
        raise BadInput(f"kind must be one of {', '.join(KINDS)}; got {ev['kind']!r}")
    for k in ("pr", "ci_run"):
        v = ev[k]
        if isinstance(v, bool) or not (v is None or isinstance(v, int)
                                       or (isinstance(v, str) and v.isdigit())):
            raise BadInput(f"{k} must be an integer or null; got {v!r}")
        ev[k] = None if v is None else int(v)
    for k in ("repo", "branch", "head", "base", "merge_sha", "crate", "version"):
        if ev[k] is not None and not isinstance(ev[k], str):
            raise BadInput(f"{k} must be a string or null; got {ev[k]!r}")
    claims = ev["claims"] if ev["claims"] is not None else {}
    if not isinstance(claims, dict):
        raise BadInput("claims must be an object or null")
    for k in ("files", "commits"):
        v = claims.get(k)
        if v is not None and (isinstance(v, bool) or not isinstance(v, int)):
            raise BadInput(f"claims.{k} must be an integer; got {v!r}")
    chk = claims.get("checks")
    if chk is not None:
        if not isinstance(chk, dict) or not all(
                k in ("pass", "fail", "pending") and isinstance(v, int) and not isinstance(v, bool)
                for k, v in chk.items()):
            raise BadInput("claims.checks must be {pass,fail,pending: integer}")
    issues = claims.get("issues")
    if issues is not None:
        if not isinstance(issues, list) or not all(
                isinstance(i, dict) and isinstance(i.get("n"), int) and not isinstance(i.get("n"), bool)
                and str(i.get("state")).lower() in ("open", "closed")
                and isinstance(i.get("reason", ""), str) for i in issues):
            raise BadInput("claims.issues must be a list of {n: integer, state: open|closed, reason?: string}")
    ev["claims"] = claims
    return ev


def load_event(arg: str, stdin=None) -> dict:
    """Read the event from a file path, "-" (stdin) or a literal JSON object."""
    if arg == "-":
        text = (stdin or sys.stdin).read()
    elif arg.lstrip().startswith("{"):
        text = arg
    elif os.path.isfile(arg):
        with open(arg) as f:
            text = f.read()
    else:
        raise BadInput(f"not a file, '-' or JSON object: {arg[:60]!r}")
    try:
        return parse_event(json.loads(text))
    except ValueError as e:
        raise BadInput(f"invalid JSON: {e}") from None


# ---------------------------------------------------------------- checks


def sha_state(claimed: str, actual: str) -> str:
    """'ok' | 'differs' | 'short' (under 10 hex) | 'notsha' (not hex)."""
    c = claimed.strip().lower()
    if not re.fullmatch(r"[0-9a-f]+", c):
        return "notsha"
    if len(c) < MIN_SHA:
        return "short"
    return "ok" if actual.lower().startswith(c) else "differs"


class Verifier:
    """Runs every claim of one event; results is a list of (status, line)."""

    def __init__(self, event: dict, runner: Runner = default_runner,
                 repos: dict | None = None, owners: dict | None = None,
                 environ: dict | None = None):
        self.ev = event
        self.claims = event["claims"]
        self.runner = runner
        self.repos = REPOS if repos is None else repos
        self.owners = OWNER_CONFIG if owners is None else owners
        self.environ = dict(os.environ if environ is None else environ)
        self.offline = self.environ.get("VERIFY_CLAIM_OFFLINE") == "1"
        self.results: list[tuple[str, str]] = []
        self.slug: str | None = None
        self.path: str | None = None
        self.gh_env: dict | None = None
        self.via = ""                    # " (via rest)" while comparing REST-sourced values
        self.last_err = ""               # full text of the last gh call (stderr, stdout)
        self.last_rc = 0
        self.rest_pr: dict | None = None
        self.rest_pr_why = ""

    # -- result helpers
    def match(self, claim: str) -> None:
        self.results.append(("MATCH", f"MATCH {claim}{self.via}"))

    def mismatch(self, claim: str, expected: object, got: object) -> None:
        self.results.append(("MISMATCH", f"MISMATCH {claim}: expected {expected}, got {got}{self.via}"))

    def unchecked(self, claim: str, reason: str) -> None:
        self.results.append(("UNCHECKED", f"UNCHECKED {claim}: {reason}"))

    def run(self, argv: list, env: dict | None = None) -> tuple:
        guard_read_only(argv)
        if self.offline and tool_is_network(argv):
            return 127, "", "offline (VERIFY_CLAIM_OFFLINE=1)"
        return self.runner(argv, env)

    @staticmethod
    def _err(err: str) -> str:
        line = (err.strip().splitlines() or ["no output"])[0]
        return line[:120]

    def compare_sha(self, claim: str, claimed: str | None, actual: str) -> None:
        st = sha_state(claimed or "", actual)
        if st == "ok":
            self.match(f"{claim} {claimed}")
        elif st == "differs":
            self.mismatch(f"{claim} {claimed}", claimed, actual)
        elif st == "short":
            self.unchecked(f"{claim} {claimed}", f"claimed sha has under {MIN_SHA} hex characters")
        else:
            self.unchecked(f"{claim} {claimed}", "not a hex sha")

    def compare_eq(self, claim: str, expected: object, got: object) -> None:
        if expected == got:
            self.match(f"{claim} {expected}")
        else:
            self.mismatch(f"{claim} {expected}", expected, got)

    # -- repo and account routing
    def resolve_repo(self) -> bool:
        repo = self.ev["repo"]
        if not repo:
            return False
        name = repo.strip()
        entry = self.repos.get(name.lower())
        if entry:
            self.slug, self.path = entry
        elif "/" in name:
            self.slug = name
            for slug, path in self.repos.values():
                if slug.lower() == name.lower():
                    self.slug, self.path = slug, path
        else:
            return False
        owner = self.slug.split("/")[0].lower()
        if owner not in self.owners:
            return False
        env = dict(self.environ)
        env.pop("GH_TOKEN", None)
        env.pop("GITHUB_TOKEN", None)
        if self.owners[owner]:
            env["GH_CONFIG_DIR"] = self.owners[owner]
        self.gh_env = env
        return True

    # -- PR claims
    def _gh_json(self, argv: list) -> tuple:
        rc, out, err = self.run(argv, self.gh_env)
        self.last_err, self.last_rc = f"{err}\n{out}", rc
        try:
            return json.loads(out), ""
        except ValueError:
            return None, self._err(err or out)

    def _rate_limited(self) -> bool:
        return bool(RATE_LIMIT.search(self.last_err))

    def _rest_get(self, path: str) -> tuple:
        """GET repos/<slug>/<path> over REST; (data, why) with data None on failure."""
        data, why = self._gh_json(["gh", "api", f"repos/{self.slug}/{path}"])
        if data is not None and self.last_rc != 0:    # gh api prints the error body as JSON
            msg = data.get("message") if isinstance(data, dict) else None
            return None, self._err(str(msg or self.last_err))
        return data, why

    def _fetch_rest_pr(self) -> dict | None:
        """The PR over REST, shaped like `gh pr view --json` output; cached. None on failure."""
        if self.rest_pr is None and not self.rest_pr_why:
            d, why = self._rest_get(f"pulls/{self.ev['pr']}")
            if not isinstance(d, dict) or "head" not in d:
                self.rest_pr_why = why or "unexpected REST reply"
                return None
            merged = bool(d.get("merged"))
            self.rest_pr = {
                "headRefOid": (d.get("head") or {}).get("sha"),
                "baseRefOid": (d.get("base") or {}).get("sha"),
                "mergeCommit": {"oid": d.get("merge_commit_sha")} if merged and d.get("merge_commit_sha") else None,
                "state": "MERGED" if merged else ("CLOSED" if d.get("state") == "closed" else "OPEN"),
                "mergedAt": d.get("merged_at"),
                "changedFiles": d.get("changed_files"),
                "commits": d.get("commits"),
            }
        return self.rest_pr

    def check_pr(self) -> None:
        ev, cl, pr = self.ev, self.claims, self.ev["pr"]
        want: dict[str, str] = {}
        state = cl.get("pr_state") or ("MERGED" if ev["kind"] == "merged" else None)
        if ev["head"]:
            want["headRefOid"] = "head"
        if ev["base"]:
            want["baseRefOid"] = "base"
        if ev["merge_sha"]:
            want["mergeCommit"] = "merge_sha"
        if state:
            want["state"] = "pr_state"
        if cl.get("merged_at"):
            want["mergedAt"] = "merged_at"
        if cl.get("files") is not None:
            want["changedFiles"] = "files"
        if cl.get("commits") is not None:
            want["commits"] = "commits"
        if want:
            data, why = self._gh_json(["gh", "pr", "view", str(pr), "--repo", self.slug,
                                       "--json", ",".join(want)])
            if data is None and self._rate_limited():
                rest = self._fetch_rest_pr()
                if rest is None:
                    why = f"{why}; REST fallback failed: {self.rest_pr_why}"
                else:
                    data, self.via = rest, " (via rest)"
            if data is None:
                for label in want.values():
                    self.unchecked(f"pr {pr} {label}", f"gh pr view failed: {why}")
            else:
                self._compare_pr(data, state)
                self.via = ""
        if cl.get("checks") is not None:
            self._check_buckets(cl["checks"])

    def _compare_pr(self, data: dict, state: str | None) -> None:
        ev, cl, pr = self.ev, self.claims, self.ev["pr"]
        if ev["head"]:
            self.compare_sha(f"pr {pr} head", ev["head"], data.get("headRefOid") or "")
        if ev["base"]:
            self.compare_sha(f"pr {pr} base", ev["base"], data.get("baseRefOid") or "")
        if ev["merge_sha"]:
            merge = (data.get("mergeCommit") or {}).get("oid") or ""
            if merge:
                self.compare_sha(f"pr {pr} merge_sha", ev["merge_sha"], merge)
            else:
                self.mismatch(f"pr {pr} merge_sha {ev['merge_sha']}", ev["merge_sha"], "no merge commit")
        if state:
            self.compare_eq(f"pr {pr} state", state.upper(), data.get("state"))
        if cl.get("merged_at"):
            want, got = cl["merged_at"].replace("+00:00", "Z"), data.get("mergedAt") or "null"
            if len(want) >= 10 and got.startswith(want):
                self.match(f"pr {pr} merged_at {cl['merged_at']}")
            else:
                self.mismatch(f"pr {pr} merged_at {cl['merged_at']}", cl["merged_at"], got)
        if cl.get("files") is not None:
            self.compare_eq(f"pr {pr} files", cl["files"], data.get("changedFiles"))
        if cl.get("commits") is not None:
            n = data.get("commits")
            self.compare_eq(f"pr {pr} commits", cl["commits"], n if isinstance(n, int) else len(n or []))

    def _check_buckets(self, want: dict) -> None:
        pr = self.ev["pr"]
        data, why = self._gh_json(["gh", "pr", "checks", str(pr), "--repo", self.slug,
                                   "--json", "bucket"])
        if not isinstance(data, list) and self._rate_limited():
            data, why = self._check_runs_rest(why)
            self.via = " (via rest)" if data is not None else ""
        if not isinstance(data, list):
            for k in want:
                self.unchecked(f"pr {pr} checks {k}", f"gh pr checks failed: {why}")
            return
        got = {"pass": 0, "fail": 0, "pending": 0}
        for row in data:
            b = row.get("bucket")
            if b in got:
                got[b] += 1
        for k, n in want.items():
            self.compare_eq(f"pr {pr} checks {k}", n, got[k])
        self.via = ""

    def _check_runs_rest(self, why: str) -> tuple:
        """Check-run buckets of the PR head over REST: (rows shaped like `gh pr checks`, why)."""
        pr = self._fetch_rest_pr()
        if pr is None:
            return None, f"{why}; REST fallback failed: {self.rest_pr_why}"
        d, rwhy = self._rest_get(f"commits/{pr['headRefOid']}/check-runs?per_page=100")
        if not isinstance(d, dict) or not isinstance(d.get("check_runs"), list):
            return None, f"{why}; REST fallback failed: {rwhy or 'unexpected REST reply'}"
        if d.get("total_count", 0) > len(d["check_runs"]):
            return None, f"{why}; REST fallback failed: more than {len(d['check_runs'])} check runs"
        rows = []
        for run in d["check_runs"]:
            if run.get("status") != "completed":
                bucket = "pending"
            elif run.get("conclusion") in ("success",):
                bucket = "pass"
            elif run.get("conclusion") in ("failure", "timed_out", "startup_failure", "action_required"):
                bucket = "fail"
            else:
                bucket = "skipping"          # neutral, skipped, cancelled, stale
            rows.append({"bucket": bucket})
        return rows, ""

    # -- issue claims
    def check_issues(self) -> None:
        for it in self.claims.get("issues") or []:
            n, label = it["n"], f"issue {it['n']}"
            if self.gh_env is None:
                self.unchecked(label, self._no_account_reason())
                continue
            d, why = self._rest_get(f"issues/{n}")
            if not isinstance(d, dict):
                self.unchecked(label, f"gh api failed: {why}")
            elif "pull_request" in d:
                self.unchecked(label, "number is a pull request, not an issue")
            else:
                self.compare_eq(f"{label} state", str(it["state"]).upper(), str(d.get("state")).upper())
                if it.get("reason"):
                    want = re.sub(r"[\s-]+", "_", it["reason"].strip().lower())
                    self._compare_reason(label, it["reason"], want, d)

    def _compare_reason(self, label: str, claimed: str, want: str, d: dict) -> None:
        got = d.get("state_reason") or "null"
        if want == got:
            self.match(f"{label} state_reason {claimed}")
        else:
            self.mismatch(f"{label} state_reason {claimed}", claimed, got)

    def _no_account_reason(self) -> str:
        return (f"repo {self.ev['repo']!r} has no mapped gh account" if self.ev["repo"]
                else "event has no repo")

    # -- local checkout claims
    def _git(self, *args: str) -> tuple:
        return self.run(["git", "-C", self.path, *args])

    def _commit(self, ref: str) -> str | None:
        """Full oid; "" when the commit is absent; None when git itself failed."""
        rc, out, _ = self._git("rev-parse", "--verify", "--quiet", f"{ref}^{{commit}}")
        if rc == 0 and out.strip():
            return out.strip()
        return "" if rc == 1 else None

    def check_local(self) -> None:
        ev, cl = self.ev, self.claims
        need_local = (ev["head"] or ev["base"] or ev["merge_sha"]
                      or cl.get("commits") is not None or cl.get("files") is not None)
        if not need_local:
            return
        if not self.path or not os.path.isdir(self.path):
            self.unchecked("local checkout", f"no local checkout for repo {ev['repo']!r}")
            return
        full: dict[str, str | None] = {}
        for field in ("head", "base"):
            ref = ev[field]
            if not ref:
                continue
            st = sha_state(ref, "")
            if st in ("short", "notsha"):
                self.compare_sha(field, ref, "")
                continue
            full[field] = self._commit(ref)
            if full[field] is None:
                self.unchecked(f"{field} {ref}", "git failed in the local checkout")
            elif full[field]:
                self.match(f"{field} {ref} exists in {os.path.basename(self.path)}")
            else:
                self.mismatch(f"{field} {ref}", "commit present in local checkout",
                              "absent (no fetch was run)")
        if ev["merge_sha"] and sha_state(ev["merge_sha"], "") != "differs":
            self.compare_sha("merge_sha", ev["merge_sha"], "")
        elif ev["merge_sha"]:
            found = self._commit(ev["merge_sha"])
            if found:
                self.match(f"merge_sha {ev['merge_sha']} exists in {os.path.basename(self.path)}")
            else:
                self.unchecked(f"merge_sha {ev['merge_sha']}",
                               "not in local checkout (it may be behind origin; no fetch was run)")
        head, base = full.get("head"), full.get("base")
        if head and base:
            rc, _, err = self._git("merge-base", "--is-ancestor", base, head)
            if rc == 0:
                self.match(f"head {ev['head']} descends from base {ev['base']}")
            elif rc == 1:
                self.mismatch(f"head {ev['head']} descends from base {ev['base']}",
                              "head descends from base", "head is not a descendant")
            else:
                self.unchecked("descendant check", self._err(err))
        for key in ("commits", "files"):
            if cl.get(key) is None:
                continue
            if not (head and base):
                self.unchecked(f"{key} {cl[key]}", "needs a head and a base present in the checkout")
            elif key == "commits":
                rc, out, err = self._git("rev-list", "--count", f"{base}..{head}")
                self._count("commits over base", cl[key], rc, out.strip(), err)
            else:
                rc, out, err = self._git("diff", "--numstat", base, head)
                n = str(len([ln for ln in out.splitlines() if ln.strip()]))
                self._count("files changed over base", cl[key], rc, n, err)

    def _count(self, label: str, want: int, rc: int, got: str, err: str) -> None:
        if rc != 0 or not got.isdigit():
            self.unchecked(f"{label} {want}", self._err(err))
        else:
            self.compare_eq(label, want, int(got))

    # -- crates.io and CI
    def check_crate(self) -> None:
        ev, cl = self.ev, self.claims
        expected = cl.get("crates_io_max") or (ev["version"] if ev["kind"] == "published" else None)
        if not ev["crate"] or not expected:
            return
        if not re.fullmatch(r"[A-Za-z0-9_-]+", ev["crate"]):
            self.unchecked(f"crate {ev['crate']}", "not a valid crate name")
            return
        rc, out, err = self.run(["curl", "-fsS", "-m", "20", "-A", USER_AGENT,
                                 f"https://crates.io/api/v1/crates/{ev['crate']}"])
        try:
            got = json.loads(out)["crate"]["max_version"]
        except (ValueError, KeyError, TypeError):
            self.unchecked(f"crates_io_max {ev['crate']}", f"crates.io lookup failed: {self._err(err or out)}")
            return
        self.compare_eq(f"crates_io_max {ev['crate']}", expected, got)

    def check_ci(self) -> None:
        ev, cl = self.ev, self.claims
        if not ev["ci_run"]:
            return
        label = f"ci_run {ev['ci_run']}"
        if not (cl.get("ci_conclusion") or cl.get("ci_head")):
            self.unchecked(label, "no ci_conclusion or ci_head claim to compare")
            return
        data, why = self._gh_json(["gh", "run", "view", str(ev["ci_run"]), "--repo", self.slug,
                                   "--json", "conclusion,headSha,status"])
        if data is None:
            self.unchecked(label, f"gh run view failed: {why}")
            return
        if cl.get("ci_conclusion"):
            got = data.get("conclusion") or data.get("status") or "unknown"
            self.compare_eq(f"{label} conclusion", cl["ci_conclusion"], got)
        if cl.get("ci_head"):
            self.compare_sha(f"{label} head", cl["ci_head"], data.get("headSha") or "")

    # -- driver
    def verify(self) -> list:
        ev = self.ev
        has_gh = ev["pr"] or ev["ci_run"] or self.claims.get("issues")
        repo_ok = self.resolve_repo()
        if has_gh and not repo_ok:
            why = self._no_account_reason()
            for label, on in ((f"pr {ev['pr']}", ev["pr"]), (f"ci_run {ev['ci_run']}", ev["ci_run"])):
                if on:
                    self.unchecked(label, why)
            self.check_issues()
        elif ev["pr"]:
            self.check_pr()
        if ev["ci_run"] and repo_ok:
            self.check_ci()
        if repo_ok:
            self.check_issues()
        if not ev["pr"]:
            self.check_local()
        self.check_crate()
        if not self.results:
            self.unchecked("event", "no checkable claims")
        return self.results


def main(argv: list | None = None, runner: Runner = default_runner, stdin=None) -> int:
    argv = sys.argv[1:] if argv is None else argv
    if len(argv) != 1 or argv[0] in ("-h", "--help"):
        print("usage: verify-claim.py EVENT   (file path, '-' for stdin, or literal JSON)",
              file=sys.stderr)
        return 2
    try:
        event = load_event(argv[0], stdin)
    except BadInput as e:
        print(f"BADINPUT {e}", file=sys.stderr)
        return 2
    results = Verifier(event, runner=runner).verify()
    for _, line in results:
        print(line)
    return 1 if any(s == "MISMATCH" for s, _ in results) else 0


if __name__ == "__main__":
    sys.exit(main())
