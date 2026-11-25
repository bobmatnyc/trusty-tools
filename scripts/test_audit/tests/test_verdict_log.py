"""Tests for the verdict log merge and the inventory CLI's idempotence (#11268).

The CLI tests build a throwaway git repo under tmp_path; they never read or
write the checkout's docs/test-audit/verdicts.jsonl.
"""
import json
import subprocess

import pytest

from test_audit.inventory import main
from test_audit.verdict_log import LogError, SCHEMA, dumps, merge, read_log, row

SCOPE = ["crates/demo/"]


def r(key, sha="s1", verdict=None, reason="", **kw):
    base = dict(key=key, body_sha=sha, rules=[], verdict=verdict, reason=reason,
                evidence="f.rs:1", reviewer="script", audit_rev="old", date="2026-01-01",
                action="none")
    return row(**{**base, **kw})


K = "demo::crates/demo/src/lib.rs::tests::a"


def test_reviewed_row_with_same_sha_is_kept_verbatim():
    old = r(K, verdict="keep", reason="covers X", reviewer="code-critic")
    fresh = r(K, rules=["R3"], reason="R3: no assertion", audit_rev="new", date="2026-02-02")
    assert merge([old], [fresh], SCOPE, "new", "2026-02-02") == [old]


def test_unreviewed_row_keeps_stamp_unless_script_fields_change():
    old = r(K)
    assert merge([old], [r(K, audit_rev="new", date="d2")], SCOPE, "new", "d2") == [old]
    flagged = r(K, rules=["R3"], reason="R3: no assertion", audit_rev="new", date="d2")
    assert merge([old], [flagged], SCOPE, "new", "d2") == [flagged]


def test_changed_body_of_a_reviewed_test_needs_rereview():
    old = r(K, verdict="weak", reason="cannot fail", reviewer="code-critic", action="#9")
    out = merge([old], [r(K, sha="s2", audit_rev="new")], SCOPE, "new", "d2")[0]
    assert (out["verdict"], out["body_sha"], out["action"]) == ("rereview", "s2", "#9")
    assert out["reason"] == "body_sha changed; was weak: cannot fail"
    again = merge([out], [r(K, sha="s2", audit_rev="newer")], SCOPE, "newer", "d3")
    assert again == [out]


def test_vanished_tests_are_marked_or_dropped_and_out_of_scope_rows_kept():
    reviewed = r(K, verdict="keep", reason="ok", reviewer="code-critic")
    unreviewed = r("demo::crates/demo/src/lib.rs::tests::b")
    other = r("x::crates/x/src/lib.rs::c", verdict="keep", reason="ok")
    out = merge([reviewed, unreviewed, other], [], SCOPE, "new", "d2")
    assert [o["key"] for o in out] == [K, other["key"]]
    assert out[0]["verdict"] == "rereview" and out[0]["reason"] == "test vanished; was keep: ok"
    assert out[1] == other
    assert merge(out, [], SCOPE, "newer", "d3") == out  # marked once, then stable


def test_log_rows_must_match_the_schema(tmp_path):
    bad = tmp_path / "v.jsonl"
    bad.write_text(json.dumps({"key": K, "body_sha": "s"}) + "\n")
    with pytest.raises(LogError):
        read_log(bad)
    with pytest.raises(LogError):
        row(key=K)
    good = tmp_path / "g.jsonl"
    good.write_text(dumps([r(K)]))
    assert tuple(read_log(good)[0]) == SCHEMA


LIB = '''
#[cfg(test)]
mod tests {
    #[test]
    fn adds() { assert_eq!(1 + 1, 2); }

    #[test]
    fn runs() { go(); }
}
'''


def git(repo, *args):
    subprocess.run(["git", "-C", str(repo), *args], check=True, capture_output=True)


@pytest.fixture
def repo(tmp_path):
    (tmp_path / "crates/demo/src").mkdir(parents=True)
    (tmp_path / "crates/demo/Cargo.toml").write_text('[package]\nname = "demo-pkg"\n')
    (tmp_path / "crates/demo/src/lib.rs").write_text(LIB)
    git(tmp_path, "init", "-q")
    git(tmp_path, "add", ".")
    git(tmp_path, "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "init")
    return tmp_path


def run(repo, *extra):
    return main(["--crates", "demo", "--root", str(repo), "--date", "2026-11-24", *extra])


def test_cli_rows_rerun_is_byte_identical_and_check_passes(repo, capsys):
    assert run(repo) == 0
    log = repo / "docs/test-audit/verdicts.jsonl"
    first = log.read_bytes()
    rows = [json.loads(x) for x in first.decode().splitlines()]
    assert [x["key"] for x in rows] == ["demo-pkg::crates/demo/src/lib.rs::tests::adds",
                                        "demo-pkg::crates/demo/src/lib.rs::tests::runs"]
    head = subprocess.run(["git", "-C", str(repo), "rev-parse", "HEAD"], capture_output=True,
                          text=True).stdout.strip()
    assert all(x["reviewer"] == "script" and x["verdict"] is None and x["audit_rev"] == head
               for x in rows)
    assert rows[1]["rules"] == ["R3"] and rows[0]["rules"] == ["R5"]
    git(repo, "add", ".")
    git(repo, "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "log")
    assert run(repo) == 0  # HEAD moved; nothing in the tree did
    assert log.read_bytes() == first
    assert run(repo, "--check") == 0
    assert "is current" in capsys.readouterr().out


def test_cli_comment_edit_changes_nothing_and_body_edit_flags_reviewed_row(repo):
    assert run(repo) == 0
    log = repo / "docs/test-audit/verdicts.jsonl"
    rows = [json.loads(x) for x in log.read_text().splitlines()]
    rows[0].update(verdict="keep", reason="pins addition", reviewer="code-critic")
    log.write_text(dumps(rows))
    reviewed = log.read_bytes()
    src = repo / "crates/demo/src/lib.rs"
    src.write_text(LIB.replace("fn adds() {", "fn adds() {\n        // a new comment\n"))
    assert run(repo) == 0
    assert log.read_bytes() == reviewed
    src.write_text(LIB.replace("1 + 1, 2", "2 + 2, 4"))
    assert run(repo, "--check") == 1
    assert run(repo) == 0
    after = [json.loads(x) for x in log.read_text().splitlines()]
    assert after[0]["verdict"] == "rereview"
    assert after[0]["reason"] == "body_sha changed; was keep: pins addition"
