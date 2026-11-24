"""Tests for scripts/architect_bundle_meta.py, the Architect bundle provenance and seed history (#10571).

The checked-in files must be current (the --check modes pass on the tree), and each --check mode must fail when its
input moves. The failure cases run against a copy of the inputs in a temp tree, never the checkout.
Run: python3 -m pytest scripts/tests/test_architect_bundle_meta.py -q -p no:cacheprovider
"""

import argparse
import importlib.util
import json
import shutil
import subprocess
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parents[1]
ROOT = SCRIPTS.parent
_spec = importlib.util.spec_from_file_location("architect_bundle_meta", SCRIPTS / "architect_bundle_meta.py")
meta = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(meta)


def args(**kw):
    base = {"commit": "", "check": True, "from_git": False, "from_repo": "", "paths": []}
    return argparse.Namespace(**{**base, **kw})


def test_checked_in_provenance_and_history_are_current():
    assert meta.provenance(args()) == 0
    assert meta.history(args()) == 0


def seed_tree(tmp_path):
    """A copy of every seed table file, the history and the seeded assets under `tmp_path`."""
    rows = meta.seed_rows(ROOT)
    fleet = tmp_path / meta.FLEET.relative_to(ROOT)
    fleet.mkdir(parents=True)
    for table in meta.FLEET.glob("seed_table*.rs"):   # #10571: the table spans several files
        shutil.copy(table, fleet / table.name)
    shutil.copy(meta.HISTORY, fleet / "seed_history.tsv")
    table = tmp_path / meta.SKILLS_TABLE.relative_to(ROOT)   # #11773: the skills' `previous` hashes
    table.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy(meta.SKILLS_TABLE, table)
    for _, asset in rows:
        dst = tmp_path / asset
        dst.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy(ROOT / asset, dst)
    return rows


def test_seed_rows_reads_every_table_file():
    dests = [d for d, _ in meta.seed_rows(ROOT)]
    assert "templates/dashboard/index.html" in dests   # seed_table.rs
    assert "scripts/fleet-poll.py" in dests            # seed_table_scripts.rs
    assert len(dests) == len(set(dests))


def test_seed_rows_include_the_content_sourced_rows():
    rows = dict(meta.seed_rows(ROOT))
    assert rows["CLAUDE.md"] == "content/instructions/architect/templates/CLAUDE.md"
    assert rows[".claude/skills/tm-fleet-check/SKILL.md"] == "content/instructions/architect/skills/tm-fleet-check.md"
    assert rows[".claude/agents/runner.md"] == "content/instructions/architect/agents/runner.md"
    skills = [d for d in rows if d.startswith(".claude/skills/")]
    assert len(skills) == len(list((ROOT / meta.CONTENT / "skills").glob("*.md")))


def test_history_records_every_skill_previous_hash(tmp_path):
    previous = meta.skill_previous(ROOT)
    assert (".claude/skills/tm-fleet-check/SKILL.md",
            "3de7f63492d03ef3d7b0ca613c04ef977b82f4349e9e3d379240c323f6b91db6") in previous
    assert len(previous) >= 40
    assert previous <= history_lines(ROOT)
    seed_tree(tmp_path)
    hist = tmp_path / meta.HISTORY.relative_to(ROOT)
    dropped = "\t".join(sorted(previous)[0])
    hist.write_text("".join(line for line in hist.read_text(encoding="utf-8").splitlines(True)
                            if line.rstrip("\n") != dropped), encoding="utf-8")
    assert meta.history(args(), root=tmp_path) == 1
    assert meta.history(args(check=False), root=tmp_path) == 0
    assert meta.history(args(), root=tmp_path) == 0


def test_history_check_fails_when_a_content_seed_changes(tmp_path):
    seed_tree(tmp_path)
    assert meta.history(args(), root=tmp_path) == 0
    (tmp_path / meta.CONTENT / "templates" / "CLAUDE.md").write_bytes(b"changed\n")
    assert meta.history(args(), root=tmp_path) == 1
    assert meta.history(args(check=False), root=tmp_path) == 0
    assert ("CLAUDE.md", meta.sha(b"changed\n")) in history_lines(tmp_path)


def test_history_check_fails_when_a_seed_changes(tmp_path):
    rows = seed_tree(tmp_path)
    assert meta.history(args(), root=tmp_path) == 0
    (tmp_path / rows[0][1]).write_bytes(b"changed\n")
    assert meta.history(args(), root=tmp_path) == 1
    assert meta.history(args(check=False), root=tmp_path) == 0   # the refresh records the new bytes
    assert meta.history(args(), root=tmp_path) == 0


def test_provenance_check_fails_when_a_pair_source_changes(monkeypatch):
    good = json.loads(meta.PROVENANCE.read_text(encoding="utf-8"))
    rel = sorted(good["files"])[0]
    stale = dict(good, files=dict(good["files"], **{rel: "0" * 64}))
    real_read = Path.read_text

    def read(self, *a, **k):
        return json.dumps(stale) if self == meta.PROVENANCE else real_read(self, *a, **k)

    monkeypatch.setattr(Path, "read_text", read)
    assert meta.provenance(args()) == 1


def test_provenance_refuses_a_malformed_commit():
    assert meta.main(["provenance", "--commit", "not-a-sha", "--check"]) == 2


def git(repo, *argv):
    subprocess.run(["git", "-C", str(repo), "-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false",
                    *argv], check=True, capture_output=True)


def commit_file(repo, rel, data):
    (repo / rel).parent.mkdir(parents=True, exist_ok=True)
    (repo / rel).write_bytes(data)
    git(repo, "add", rel)
    git(repo, "commit", "-q", "-m", rel)


def history_lines(root):
    text = (root / meta.HISTORY.relative_to(ROOT)).read_text(encoding="utf-8")
    return {tuple(line.split("\t")) for line in text.splitlines() if line and not line.startswith("#")}


def test_history_from_repo_adds_every_live_script_version(tmp_path):
    tree, live = tmp_path / "tree", tmp_path / "live"
    tree.mkdir()
    seed_tree(tree)
    live.mkdir()
    git(live, "init", "-q")
    versions = [b"v1\n", b"v2\n", b"v3\n"]
    for data in versions:
        commit_file(live, "scripts/fleet-poll.py", data)
    commit_file(live, "scripts/input-state.py", b"input\n")
    commit_file(live, "scripts/not-seeded.py", b"x\n")   # not a seed row: never recorded
    commit_file(live, "local/state.md", b"owner\n")        # seeded, not a script: never recorded
    git(live, "checkout", "-q", "-b", "side")
    commit_file(live, "scripts/fleet-poll.py", b"side\n")  # a version on another branch counts too
    before = history_lines(tree)

    assert meta.history(args(check=False, from_repo=str(live)), root=tree) == 0
    added = history_lines(tree) - before
    want = {("scripts/fleet-poll.py", meta.sha(d)) for d in versions + [b"side\n"]}
    want.add(("scripts/input-state.py", meta.sha(b"input\n")))
    assert added == want

    seed_tree_again = tmp_path / "tree2"
    seed_tree_again.mkdir()
    seed_tree(seed_tree_again)
    assert meta.history(args(check=False, from_repo=str(live), paths=["scripts/input-state.py"]),
                        root=seed_tree_again) == 0
    assert history_lines(seed_tree_again) - before == {("scripts/input-state.py", meta.sha(b"input\n"))}


def test_history_from_repo_refuses_bad_input(tmp_path):
    assert meta.main(["history", "--check", "--from-repo", str(tmp_path)]) == 2   # not a git repo
    git(tmp_path, "init", "-q")
    assert meta.main(["history", "--check", "--from-repo", str(tmp_path), "--paths", "scripts/nope.py"]) == 2
    assert meta.main(["history", "--check", "--from-repo", str(tmp_path), "--paths", "local/state.md"]) == 2
    assert meta.main(["history", "--check", "--paths", "scripts/fleet-poll.py"]) == 2
