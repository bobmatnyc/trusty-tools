"""Tests for scripts/verify-claim.py (T07). Every gh, git and curl call goes
through a fake runner; nothing touches the network or a real checkout."""

import contextlib
import importlib.util
import io
import json
import os
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
_spec = importlib.util.spec_from_file_location("verify_claim", os.path.join(HERE, "verify-claim.py"))
vc = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(vc)

H = "5d200df7b31ed09ecdc86195ad86037f70435567"
B = "7d7219f7566df434cfb9ad15400fa6632ae117c8"
M = "8fac7d5269f5f006d2919ac3f3908c9f9699b33f"
ENV = {"GH_TOKEN": "t1", "GITHUB_TOKEN": "t2", "GH_CONFIG_DIR": "/default-gh", "PATH": "/bin"}


class Fake:
    """Fake runner: handlers is a list of (predicate(argv), (rc, out, err))."""

    def __init__(self, *handlers):
        self.handlers = list(handlers)
        self.calls = []

    def __call__(self, argv, env=None):
        self.calls.append((list(argv), env))
        for pred, reply in self.handlers:
            if pred(argv):
                return reply
        return 1, "", "no fake handler"


def has(*words):
    return lambda argv: all(w in argv for w in words)


def pr_view(data, rc=0, err=""):
    return has("pr", "view"), (rc, json.dumps(data) if data is not None else "", err)


def ev(**kw):
    base = {"kind": "clearance_request"}
    base.update(kw)
    return vc.parse_event(base)


def verify(event, *handlers, repos=None, environ=ENV):
    fake = Fake(*handlers)
    v = vc.Verifier(event, runner=fake, repos=repos, environ=environ)
    return v.verify(), fake


def lines(results):
    return [line for _, line in results]


class PrClaims(unittest.TestCase):
    def test_all_match_and_exit_0(self):
        data = {"headRefOid": H, "baseRefOid": B, "mergeCommit": {"oid": M}, "state": "MERGED",
                "mergedAt": "2026-10-09T04:27:25Z", "changedFiles": 3, "commits": [{}]}
        event = ev(kind="merged", repo="WEBAPP", pr=3326, head=H[:12], base=B[:10], merge_sha=M,
                   claims={"merged_at": "2026-10-09T04:27", "files": 3, "commits": 1,
                           "checks": {"pass": 2, "fail": 0}})
        checks = (has("pr", "checks"), (0, json.dumps([{"bucket": "pass"}, {"bucket": "pass"},
                                                         {"bucket": "skipping"}]), ""))
        res, fake = verify(event, pr_view(data), checks)
        self.assertEqual({s for s, _ in res}, {"MATCH"}, lines(res))
        self.assertEqual(len(res), 9)
        self.assertIn(f"MATCH pr 3326 head {H[:12]}", lines(res))
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            code = vc.main([json.dumps(event)], runner=Fake(pr_view(data), checks))
        self.assertEqual(code, 0)

    def test_mismatch_lines_and_exit_1(self):
        data = {"headRefOid": B, "state": "OPEN"}
        event = ev(kind="merged", repo="trusty-tools", pr=9492, head=H)
        res, _ = verify(event, pr_view(data))
        self.assertIn(f"MISMATCH pr 9492 head {H}: expected {H}, got {B}", lines(res))
        self.assertIn("MISMATCH pr 9492 state MERGED: expected MERGED, got OPEN", lines(res))
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            code = vc.main([json.dumps(event)], runner=Fake(pr_view(data)))
        self.assertEqual(code, 1)
        self.assertIn("MISMATCH pr 9492 head", buf.getvalue())

    def test_short_sha_and_failed_gh_are_unchecked_not_mismatch(self):
        res, _ = verify(ev(repo="WEBAPP", pr=1, head="5d200df7b", base=B),
                        pr_view({"headRefOid": H, "baseRefOid": B}))
        self.assertEqual(lines(res)[0], "UNCHECKED pr 1 head 5d200df7b: claimed sha has under 10 hex characters")
        self.assertEqual(lines(res)[1], f"MATCH pr 1 base {B}")
        res, _ = verify(ev(repo="WEBAPP", pr=1, head=H), pr_view(None, rc=1, err="GraphQL: API rate limit\nmore"))
        self.assertEqual(lines(res), ["UNCHECKED pr 1 head: gh pr view failed: GraphQL: API rate limit; "
                                      "REST fallback failed: no fake handler"])

    def test_no_merge_commit_is_a_mismatch(self):
        res, _ = verify(ev(kind="merged", repo="WEBAPP", pr=7, merge_sha=M),
                        pr_view({"mergeCommit": None, "state": "OPEN"}))
        self.assertIn("MISMATCH pr 7 merge_sha " + M, lines(res)[0])
        self.assertIn("got no merge commit", lines(res)[0])

    def test_repo_without_account_is_unchecked(self):
        res, fake = verify(ev(repo="someone-else/repo", pr=5, head=H))
        self.assertEqual(lines(res), ["UNCHECKED pr 5: repo 'someone-else/repo' has no mapped gh account"])
        self.assertEqual(fake.calls, [])
        res, _ = verify(ev(pr=5, head=H))
        self.assertEqual(lines(res), ["UNCHECKED pr 5: event has no repo"])


class AccountRouting(unittest.TestCase):
    def _env_for(self, repo):
        _, fake = verify(ev(repo=repo, pr=1, head=H), pr_view({"headRefOid": H}))
        self.assertEqual(fake.calls[0][0][:3], ["gh", "pr", "view"])
        self.assertIn(repo.split("/")[-1].lower() if "/" in repo else "", "".join(fake.calls[0][0]).lower())
        return fake.calls[0][1]

    def test_employer_repo_uses_employer_config_and_drops_tokens(self):
        env = self._env_for("employer-org/WEBAPP")
        self.assertEqual(env["GH_CONFIG_DIR"], os.path.expanduser("~/.config/gh-employer"))
        self.assertNotIn("GH_TOKEN", env)
        self.assertNotIn("GITHUB_TOKEN", env)

    def test_bare_webapp_name_routes_to_employer(self):
        env = self._env_for("webapp")
        self.assertTrue(env["GH_CONFIG_DIR"].endswith("/.config/gh-employer"))

    def test_other_orgs_use_default_config_and_drop_tokens(self):
        for repo in ("example-org/trusty-tools", "consult-org/x", "travel-org/y", "side-project"):
            env = self._env_for(repo)
            self.assertEqual(env["GH_CONFIG_DIR"], "/default-gh", repo)
            self.assertNotIn("GH_TOKEN", env)
            self.assertNotIn("GITHUB_TOKEN", env)


class LocalClaims(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.repos = {"trusty-tools": ("example-org/trusty-tools", self.tmp.name)}

    def git(self, **over):
        """Handlers for a checkout where both commits exist, 2 commits, 4 files."""
        h = {"head": (0, H + "\n", ""), "base": (0, B + "\n", ""), "anc": (0, "", ""),
             "count": (0, "2\n", ""), "diff": (0, "1\t0\ta\n2\t1\tb\n-\t-\tc.bin\n0\t3\td\n", "")}
        h.update(over)
        return [
            (lambda a: "rev-parse" in a and a[-1].startswith(H[:10]), h["head"]),
            (lambda a: "rev-parse" in a and a[-1].startswith(B[:10]), h["base"]),
            (has("merge-base", "--is-ancestor"), h["anc"]),
            (has("rev-list", "--count"), h["count"]),
            (has("diff", "--numstat"), h["diff"]),
        ]

    def local_event(self, **kw):
        return ev(repo="trusty-tools", head=H[:10], base=B[:10],
                  claims={"commits": 2, "files": 4}, **kw)

    def test_match(self):
        res, _ = verify(self.local_event(), *self.git(), repos=self.repos)
        self.assertEqual({s for s, _ in res}, {"MATCH"}, lines(res))
        self.assertEqual(len(res), 5)

    def test_absent_commit_not_descendant_and_wrong_counts_mismatch(self):
        res, _ = verify(self.local_event(), *self.git(head=(1, "", ""), anc=(1, "", ""),
                                                        count=(0, "3\n", ""), diff=(0, "x\n", "")),
                        repos=self.repos)
        out = lines(res)
        self.assertIn(f"MISMATCH head {H[:10]}: expected commit present in local checkout, got absent (no fetch was run)", out)
        # head absent: the descendant and count checks cannot run.
        self.assertEqual(sum(1 for s, _ in res if s == "UNCHECKED"), 2, out)
        res, _ = verify(self.local_event(), *self.git(anc=(1, "", ""), count=(0, "3\n", ""),
                                                        diff=(0, "x\n", "")), repos=self.repos)
        out = lines(res)
        self.assertTrue(any(l.startswith("MISMATCH head") and "not a descendant" in l for l in out), out)
        self.assertIn("MISMATCH commits over base 2: expected 2, got 3", out)
        self.assertIn("MISMATCH files changed over base 4: expected 4, got 1", out)

    def test_git_failure_is_unchecked(self):
        res, _ = verify(self.local_event(), *self.git(head=(128, "", "not a git repository")),
                        repos=self.repos)
        self.assertEqual(lines(res)[0], f"UNCHECKED head {H[:10]}: git failed in the local checkout")

    def test_unknown_checkout_and_merge_sha_not_local(self):
        res, _ = verify(ev(repo="someone/else", head=H), repos=self.repos)
        self.assertEqual(lines(res), ["UNCHECKED local checkout: no local checkout for repo 'someone/else'"])
        res, _ = verify(ev(kind="merged", repo="trusty-tools", merge_sha=M),
                        (has("rev-parse"), (1, "", "")), repos=self.repos)
        self.assertIn("UNCHECKED merge_sha " + M, lines(res)[0])


class CrateAndCi(unittest.TestCase):
    def curl(self, version):
        return has("curl"), (0, json.dumps({"crate": {"max_version": version}}), "")

    def test_published_match_and_user_agent(self):
        res, fake = verify(ev(kind="published", crate="trusty-common", version="0.55.1"), self.curl("0.55.1"))
        self.assertEqual(lines(res), ["MATCH crates_io_max trusty-common 0.55.1"])
        argv = fake.calls[0][0]
        self.assertEqual(argv[argv.index("-A") + 1], "trusty-architect")
        self.assertEqual(argv[-1], "https://crates.io/api/v1/crates/trusty-common")

    def test_published_mismatch_and_unreadable(self):
        res, _ = verify(ev(kind="published", crate="trusty-common", version="0.55.1"), self.curl("0.55.0"))
        self.assertEqual(lines(res), ["MISMATCH crates_io_max trusty-common 0.55.1: expected 0.55.1, got 0.55.0"])
        res, _ = verify(ev(kind="published", crate="trusty-common", version="0.55.1"),
                        (has("curl"), (22, "", "HTTP 404")))
        self.assertTrue(lines(res)[0].startswith("UNCHECKED crates_io_max trusty-common: crates.io lookup failed"))

    def test_clearance_checks_crates_io_only_when_claimed(self):
        res, fake = verify(ev(crate="trusty-common", version="0.55.1"))
        self.assertEqual(lines(res), ["UNCHECKED event: no checkable claims"])
        self.assertEqual(fake.calls, [])
        res, _ = verify(ev(crate="trusty-common", version="0.55.1", claims={"crates_io_max": "0.55.0"}),
                        self.curl("0.55.0"))
        self.assertEqual(lines(res), ["MATCH crates_io_max trusty-common 0.55.0"])

    def test_ci_run(self):
        run = has("run", "view"), (0, json.dumps({"conclusion": "success", "headSha": B}), "")
        res, _ = verify(ev(repo="trusty-tools", ci_run=1003,
                           claims={"ci_conclusion": "success", "ci_head": B[:10]}), run)
        self.assertEqual({s for s, _ in res}, {"MATCH"}, lines(res))
        res, _ = verify(ev(repo="trusty-tools", ci_run=1, claims={"ci_conclusion": "failure"}), run)
        self.assertEqual(lines(res), ["MISMATCH ci_run 1 conclusion failure: expected failure, got success"])
        res, _ = verify(ev(repo="trusty-tools", ci_run=1), run)
        self.assertTrue(lines(res)[0].startswith("UNCHECKED ci_run 1:"))


RATE = "GraphQL: API rate limit already exceeded for user ID 123.\nmore"


def rest(path_word, data, rc=0, err=""):
    """Fake `gh api repos/<o>/<r>/<path_word>/...` reply."""
    return (lambda argv: argv[:2] == ["gh", "api"] and f"/{path_word}" in argv[2]), \
        (rc, json.dumps(data) if data is not None else "", err)


REST_PR = {"head": {"sha": H}, "base": {"sha": B}, "merged": True, "merge_commit_sha": M, "state": "closed",
           "merged_at": "2026-10-09T04:27:25Z", "changed_files": 3, "commits": 1}


class RestFallback(unittest.TestCase):
    def test_graphql_rate_limit_falls_back_to_rest_for_the_same_fields(self):
        event = ev(kind="merged", repo="trusty-tools", pr=9492, head=H[:12], base=B[:10], merge_sha=M,
                   claims={"merged_at": "2026-10-09T04:27", "files": 3, "commits": 1})
        res, fake = verify(event, pr_view(None, rc=1, err=RATE), rest("pulls", REST_PR))
        self.assertEqual({s for s, _ in res}, {"MATCH"}, lines(res))
        self.assertEqual(len(res), 7)
        self.assertTrue(all(ln.endswith(" (via rest)") for ln in lines(res)), lines(res))
        rest_call = [c for c in fake.calls if c[0][:2] == ["gh", "api"]][0]
        self.assertEqual(rest_call[0][2], "repos/example-org/trusty-tools/pulls/9492")
        self.assertNotIn("GH_TOKEN", rest_call[1])
        self.assertNotIn("GITHUB_TOKEN", rest_call[1])

    def test_rest_states_and_mismatch_keep_their_shape(self):
        open_pr = dict(REST_PR, merged=False, state="open", merged_at=None)
        res, _ = verify(ev(kind="merged", repo="trusty-tools", pr=1, head=B),
                        pr_view(None, rc=1, err=RATE), rest("pulls", open_pr))
        self.assertIn(f"MISMATCH pr 1 head {B}: expected {B}, got {H} (via rest)", lines(res))
        self.assertIn("MISMATCH pr 1 state MERGED: expected MERGED, got OPEN (via rest)", lines(res))
        res, _ = verify(ev(repo="trusty-tools", pr=1, claims={"pr_state": "CLOSED"}),
                        pr_view(None, rc=1, err=RATE), rest("pulls", dict(REST_PR, merged=False, state="closed")))
        self.assertEqual(lines(res), ["MATCH pr 1 state CLOSED (via rest)"])

    def test_checks_fall_back_to_check_runs_of_the_head(self):
        runs = {"total_count": 4, "check_runs": [
            {"status": "completed", "conclusion": "success"}, {"status": "completed", "conclusion": "neutral"},
            {"status": "completed", "conclusion": "failure"}, {"status": "in_progress", "conclusion": None}]}
        res, fake = verify(ev(repo="trusty-tools", pr=1, claims={"checks": {"pass": 1, "fail": 1, "pending": 1}}),
                           (has("pr", "checks"), (1, "", RATE)),
                           rest("check-runs", runs), rest("pulls", REST_PR))
        self.assertEqual(lines(res), ["MATCH pr 1 checks pass 1 (via rest)", "MATCH pr 1 checks fail 1 (via rest)",
                                      "MATCH pr 1 checks pending 1 (via rest)"])
        self.assertIn(f"repos/example-org/trusty-tools/commits/{H}/check-runs?per_page=100",
                      [c[0][2] for c in fake.calls if c[0][:2] == ["gh", "api"]])

    def test_rest_failure_stays_unchecked_never_a_value(self):
        res, _ = verify(ev(repo="trusty-tools", pr=1, head=H, claims={"checks": {"pass": 1}}),
                        (has("pr", "checks"), (1, "", RATE)), pr_view(None, rc=1, err=RATE),
                        rest("pulls", None, rc=1, err="HTTP 403: API rate limit exceeded"))
        self.assertEqual(len(res), 2)
        for ln in lines(res):
            self.assertTrue(ln.startswith("UNCHECKED pr 1 "), ln)
            self.assertIn("REST fallback failed: HTTP 403", ln)
        self.assertEqual({s for s, _ in res}, {"UNCHECKED"})

    def test_rest_error_body_with_nonzero_exit_is_unchecked(self):
        res, _ = verify(ev(repo="trusty-tools", pr=1, head=H), pr_view(None, rc=1, err=RATE),
                        rest("pulls", {"message": "API rate limit exceeded for user"}, rc=1))
        self.assertIn("REST fallback failed: API rate limit exceeded for user", lines(res)[0])
        res, _ = verify(ev(repo="trusty-tools", claims={"issues": [{"n": 5, "state": "open"}]}),
                        rest("issues", {"message": "Not Found"}, rc=1))
        self.assertEqual(lines(res), ["UNCHECKED issue 5: gh api failed: Not Found"])

    def test_other_gh_errors_do_not_trigger_rest(self):
        res, fake = verify(ev(repo="trusty-tools", pr=1, head=H),
                           pr_view(None, rc=1, err="Could not resolve to a PullRequest"), rest("pulls", REST_PR))
        self.assertEqual(lines(res), ["UNCHECKED pr 1 head: gh pr view failed: Could not resolve to a PullRequest"])
        self.assertFalse([c for c in fake.calls if c[0][:2] == ["gh", "api"]])


class IssueClaims(unittest.TestCase):
    def fixture_event(self):
        fx = [json.loads(ln) for ln in open(os.path.join(HERE, "fixtures", "event-extractor.jsonl"))]
        f12 = next(f for f in fx if f["id"] == "f12")
        self.assertIn("#9310 CLOSED (completed)", f12["text"])
        e = f12["expected"]
        return ev(kind=e["kind"], repo=e["repo"], claims=e["claims"])

    def test_closed_completed_matches_via_rest_issue_endpoint(self):
        res, fake = verify(self.fixture_event(),
                           rest("issues", {"state": "closed", "state_reason": "completed"}))
        self.assertEqual(lines(res), ["MATCH issue 9310 state CLOSED", "MATCH issue 9310 state_reason completed"])
        self.assertEqual(fake.calls[0][0], ["gh", "api", "repos/example-org/trusty-tools/issues/9310"])

    def test_wrong_state_or_reason_mismatches_and_open_issue(self):
        res, _ = verify(self.fixture_event(), rest("issues", {"state": "closed", "state_reason": "not_planned"}))
        self.assertEqual(lines(res)[1], "MISMATCH issue 9310 state_reason completed: expected completed, got not_planned")
        res, _ = verify(self.fixture_event(), rest("issues", {"state": "open", "state_reason": None}))
        self.assertEqual(lines(res)[0], "MISMATCH issue 9310 state CLOSED: expected CLOSED, got OPEN")
        res, _ = verify(ev(repo="trusty-tools", claims={"issues": [{"n": 5, "state": "open"}]}),
                        rest("issues", {"state": "open", "state_reason": "reopened"}))
        self.assertEqual(lines(res), ["MATCH issue 5 state OPEN"])
        res, _ = verify(ev(repo="trusty-tools", claims={"issues": [{"n": 5, "state": "closed", "reason": "not planned"}]}),
                        rest("issues", {"state": "closed", "state_reason": "not_planned"}))
        self.assertEqual({s for s, _ in res}, {"MATCH"}, lines(res))

    def test_unverifiable_issue_claims_are_unchecked(self):
        res, _ = verify(self.fixture_event(), rest("issues", None, rc=1, err="HTTP 404: Not Found"))
        self.assertEqual(lines(res), ["UNCHECKED issue 9310: gh api failed: HTTP 404: Not Found"])
        res, _ = verify(self.fixture_event(), rest("issues", {"state": "open", "pull_request": {}}))
        self.assertEqual(lines(res), ["UNCHECKED issue 9310: number is a pull request, not an issue"])
        res, fake = verify(ev(repo="someone-else/x", claims={"issues": [{"n": 1, "state": "open"}]}))
        self.assertEqual(lines(res), ["UNCHECKED issue 1: repo 'someone-else/x' has no mapped gh account"])
        self.assertEqual(fake.calls, [])

    def test_bad_issue_claim_shape_exits_2(self):
        for claims in ({"issues": "9310"}, {"issues": [{"n": "x", "state": "open"}]},
                       {"issues": [{"n": 1, "state": "weird"}]}, {"issues": [5]}):
            with self.assertRaises(vc.BadInput, msg=claims):
                ev(claims=claims)


class InputAndGuard(unittest.TestCase):
    def run_main(self, arg, stdin=None):
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = vc.main([arg], runner=Fake(), stdin=stdin)
        return code, out.getvalue(), err.getvalue()

    def test_bad_input_exits_2(self):
        for arg in ("nope", "{not json", "[1]", '{"kind":"bogus"}', '{"kind":"fyi","pr":"x"}',
                    '{"kind":"fyi","claims":{"files":"4"}}', '{"kind":"fyi","claims":{"checks":{"green":1}}}'):
            code, out, err = self.run_main(arg)
            self.assertEqual((code, out), (2, ""), arg)
            self.assertIn("BADINPUT", err)
        self.assertEqual(vc.main([], runner=Fake()), 2)

    def test_file_stdin_and_literal_inputs(self):
        text = json.dumps({"kind": "fyi"})
        self.assertEqual(self.run_main("-", stdin=io.StringIO(text))[:2], (0, "UNCHECKED event: no checkable claims\n"))
        self.assertEqual(self.run_main(text)[0], 0)
        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
            f.write(text)
        self.addCleanup(os.unlink, f.name)
        self.assertEqual(self.run_main(f.name)[0], 0)

    def test_guard_refuses_writes(self):
        v = vc.Verifier(ev(), runner=Fake(), environ=ENV)
        for argv in (["gh", "pr", "merge", "1"], ["gh", "pr", "comment", "1"], ["gh", "api", "x"],
                     ["git", "-C", "/x", "push"], ["git", "commit"], ["rm", "-rf", "x"],
                     ["curl", "-X", "POST", "https://crates.io/api/v1/crates/x"],
                     ["curl", "https://example.com/"]):
            with self.assertRaises(PermissionError, msg=argv):
                v.run(argv)
        v.run(["gh", "pr", "view", "1"])  # allowed
        for path in ("repos/o/r/pulls/1", "repos/o/r/issues/1", "repos/o/r/commits/abc123/check-runs?per_page=100"):
            v.run(["gh", "api", path])  # allowed: read-only REST GET

    def test_offline_skips_gh_and_curl_only(self):
        fake = Fake()
        v = vc.Verifier(ev(kind="published", repo="trusty-tools", pr=1, head=H, crate="c", version="1.0.0"),
                        runner=fake, environ={**ENV, "VERIFY_CLAIM_OFFLINE": "1"})
        res = v.verify()
        self.assertEqual(fake.calls, [])
        self.assertTrue(all(s == "UNCHECKED" for s, _ in res), lines(res))
        self.assertIn("offline", lines(res)[0])


if __name__ == "__main__":
    unittest.main()
