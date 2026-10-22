"""Tests for scripts/architect_export.py, the Architect export-and-scrub tool (#9171).

Every value here is synthetic (alice, carol, example-org, a@example.com). The
tool runs as a subprocess, so each case sees the real CLI, exit code and output.
Run: python3 -m pytest scripts/tests/test_architect_export.py -q -p no:cacheprovider
"""

import os
import subprocess
import sys
from pathlib import Path

import pytest

SCRIPTS = Path(__file__).resolve().parents[1]
TOOL = SCRIPTS / "architect_export.py"
ENV_KEYS = ("ARCHITECT_LEAK_DENY_FILE", "ARCHITECT_EXPORT_REWRITE_FILE", "ARCHITECT_LEAK_PATTERNS")
TOKEN = "ghp_" + "A" * 36  # built at runtime so no scanner flags this file

MANIFEST = (
    "# synthetic starter\n"
    "scripts/a.py\n"
    ".claude/skills/demo/SKILL.md\tskills/demo.md\n"
    "templates/owner/**\ttemplates/local/owner\n"
)
TREE = {
    "scripts/a.py": "ROOT = '/Users/alice/work'\nwho = 'alice'\n",
    ".claude/skills/demo/SKILL.md": "# demo skill\n",
    "templates/owner/README.md": "# owner model template\n",
    "templates/owner/deep/notes.md": "notes\n",
    "unlisted.py": "print('not listed')\n",
    "local/secret.md": "instance data\n",
    "owner/profile.md": "instance data\n",
    "records/r.md": "instance data\n",
    "inbox/events.jsonl": "{}\n",
    ".env.local": "K=V\n",
    ".mcp.json": "{}\n",
    "docs/briefs/b.md": "brief\n",
    "docs/guide.md": "guide\n",
}
STAGED = {"scripts/a.py", "skills/demo.md", "templates/local/owner/README.md", "templates/local/owner/deep/notes.md"}


class World:
    def __init__(self, root):
        self.root = root
        self.src = root / "src"
        self.staging = root / "staging"
        self.manifest = root / "manifest.txt"
        self.deny = root / "deny.txt"
        self.rewrite = root / "rewrite.txt"
        for rel, text in TREE.items():
            self.write(rel, text)
        self.manifest.write_text(MANIFEST)
        self.deny.write_text("# private names\ncarol\n")
        self.rewrite.write_text("# FROM<TAB>TO\nalice\tthe-owner\n/Users/alice\t~\n")

    def write(self, rel, text):
        path = self.src / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)
        return path

    def run(self, *extra, deny=True, rewrite=True, env=None):
        args = [sys.executable, str(TOOL), "--source", self.src, "--staging", self.staging, "--manifest", self.manifest]
        if deny:
            args += ["--deny-file", self.deny]
        if rewrite:
            args += ["--rewrite-file", self.rewrite]
        clean = {k: v for k, v in os.environ.items() if k not in ENV_KEYS}
        clean.update(env or {})
        proc = subprocess.run([str(a) for a in args + list(extra)], capture_output=True, text=True, env=clean)
        proc.output = proc.stdout + proc.stderr
        return proc

    def staged(self):
        return {p.relative_to(self.staging).as_posix() for p in self.staging.rglob("*") if p.is_file()}


@pytest.fixture
def world(tmp_path):
    return World(tmp_path)


def test_only_listed_files_copied(world):
    proc = world.run()
    assert proc.returncode == 0, proc.output
    assert world.staged() == STAGED
    assert (world.staging / "skills/demo.md").read_text() == "# demo skill\n"


@pytest.mark.parametrize(
    "rel",
    ["local/secret.md", "owner/profile.md", "records/r.md", "inbox/events.jsonl", ".env.local", ".mcp.json",
     "docs/briefs/b.md", "LOCAL/secret.md", "local/**", "scripts/x.bak"],
)
def test_forbidden_manifest_path_refused(world, rel):
    world.manifest.write_text(MANIFEST + rel + "\n")
    proc = world.run()
    assert proc.returncode == 2, proc.output
    assert "forbidden path" in proc.stderr
    assert not world.staging.exists()


def test_forbidden_dest_path_refused(world):
    world.manifest.write_text(MANIFEST + "docs/guide.md\tlocal/guide.md\n")
    proc = world.run()
    assert proc.returncode == 2, proc.output
    assert "forbidden path" in proc.stderr


def test_forbidden_path_reached_by_glob(world):
    world.manifest.write_text(MANIFEST + "docs/**\n")
    proc = world.run()
    assert proc.returncode == 2, proc.output
    assert "forbidden path" in proc.stderr
    assert not world.staging.exists()


@pytest.mark.parametrize("target", ["outside", "inside-denied"])
def test_symlink_refused(world, target):
    if target == "outside":
        (world.root / "outside.md").write_text("outside the source\n")
        (world.src / "scripts/link.md").symlink_to(world.root / "outside.md")
        world.manifest.write_text(MANIFEST + "scripts/link.md\n")
    else:
        (world.src / "templates/owner/link.md").symlink_to(world.src / "local/secret.md")
    proc = world.run()
    assert proc.returncode == 2, proc.output
    assert "symlink" in proc.stderr
    assert not world.staging.exists()


@pytest.mark.parametrize("row", ["scripts/../local/secret.md", "../outside.md", "/etc/hosts", "scripts/a.py\t../x.py"])
def test_escaping_path_refused(world, row):
    world.manifest.write_text(MANIFEST + row + "\n")
    proc = world.run()
    assert proc.returncode == 2, proc.output
    assert "escapes" in proc.stderr
    assert not world.staging.exists()


def test_missing_listed_source_file_refused(world):
    world.manifest.write_text(MANIFEST + "scripts/gone.py\n")
    proc = world.run()
    assert proc.returncode == 2, proc.output
    assert "scripts/gone.py" in proc.stderr and "not a regular file" in proc.stderr


def test_rewrite_applied_longest_first(world):
    proc = world.run()
    assert proc.returncode == 0, proc.output
    assert (world.staging / "scripts/a.py").read_text() == "ROOT = '~/work'\nwho = 'the-owner'\n"
    assert "scripts/a.py (2 rewrite(s))" in proc.stdout
    assert "skills/demo.md (0 rewrite(s))" in proc.stdout


@pytest.mark.parametrize(
    "line,cls,value",
    [
        ("mail someone@corp.io", "EMAIL", "someone"),
        ("ssh box.tail1234.ts.net", "HOSTNAME", "tail1234"),
        ("cat ~/.config/gh-realacct/hosts.yml", "GH_CONFIG_DIR", "realacct"),
        ("ssh 100.101.102.103", "TAILNET_IP", "100.101"),
        ("TOKEN = '" + TOKEN + "'", "SECRET", TOKEN),
    ],
)
def test_residue_exits_1_naming_class_not_value(world, line, cls, value):
    world.write("scripts/a.py", "ok = 1\n" + line + "\n")
    proc = world.run()
    assert proc.returncode == 1, proc.output
    assert f"RESIDUE {cls}: scripts/a.py:2" in proc.stderr
    assert value not in proc.output
    assert not world.staging.exists()


def test_residue_in_staged_path_withholds_the_path(world):
    world.write("templates/owner/someone@corp.io.md", "x\n")
    proc = world.run()
    assert proc.returncode == 1, proc.output
    assert "RESIDUE EMAIL: path of manifest line 4" in proc.stderr
    assert "corp.io" not in proc.output


def test_deny_file_loaded_and_hits(world):
    world.write("scripts/a.py", "ask Carol first\n")
    proc = world.run()
    assert proc.returncode == 1, proc.output
    assert "RESIDUE OWNER_NAME: scripts/a.py:1" in proc.stderr
    assert "Carol" not in proc.output


def test_deny_file_read_from_env(world):
    world.write("scripts/a.py", "ask carol first\n")
    proc = world.run(deny=False, env={"ARCHITECT_LEAK_DENY_FILE": str(world.deny)})
    assert proc.returncode == 1, proc.output
    assert "RESIDUE OWNER_NAME: scripts/a.py:1" in proc.stderr


def test_deny_file_unset_fails_closed(world):
    proc = world.run(deny=False)
    assert proc.returncode == 2, proc.output
    assert "deny file is required" in proc.stderr
    assert not world.staging.exists()


@pytest.mark.parametrize(
    "case,reason",
    [("missing", "deny file missing"), ("directory", "deny file missing"), ("empty", "holds no names"),
     ("comments", "holds no names"), ("charset", "a name holds only")],
)
def test_deny_file_missing_or_empty_fails_closed(world, case, reason):
    if case == "missing":
        world.deny.unlink()
    elif case == "directory":
        world.deny.unlink()
        world.deny.mkdir()
    else:
        world.deny.write_text({"empty": "", "comments": "# none\n\n", "charset": "a/b\n"}[case])
    proc = world.run()
    assert proc.returncode == 2, proc.output
    assert reason in proc.stderr
    assert not world.staging.exists()


@pytest.mark.parametrize(
    "case,reason",
    [("missing", "rewrite file missing"), ("empty", "holds no rows"), ("no-tab", "expected FROM<TAB>TO")],
)
def test_rewrite_file_bad_fails_closed(world, case, reason):
    if case == "missing":
        world.rewrite.unlink()
    else:
        world.rewrite.write_text({"empty": "# only a comment\n", "no-tab": "alice the-owner\n"}[case])
    proc = world.run()
    assert proc.returncode == 2, proc.output
    assert reason in proc.stderr
    assert not world.staging.exists()


def test_rewrite_key_residue_caught_without_deny_entry(world):
    world.write("scripts/a.py", "who = 'ALICE'\n")
    proc = world.run()
    assert proc.returncode == 1, proc.output
    assert "RESIDUE REWRITE_KEY: scripts/a.py:1" in proc.stderr
    assert "ALICE" not in proc.output


def test_dry_run_writes_nothing_and_prints_counts(world):
    proc = world.run("--dry-run")
    assert proc.returncode == 0, proc.output
    assert not world.staging.exists()
    assert "would stage scripts/a.py (2 rewrite(s))" in proc.stdout
    assert "dry run" in proc.stdout
    world.write("scripts/a.py", "mail someone@corp.io\n")
    proc = world.run("--dry-run")
    assert proc.returncode == 1, proc.output
    assert not world.staging.exists()


def test_staging_and_source_preconditions(world, tmp_path):
    world.staging.mkdir()
    (world.staging / "old.txt").write_text("x")
    assert world.run().returncode == 2
    proc = world.run("--staging", world.src / "out")
    assert proc.returncode == 2 and "inside the source" in proc.stderr
    assert not (world.src / "out").exists()
    proc = world.run("--source", tmp_path / "nope", "--staging", tmp_path / "s2")
    assert proc.returncode == 2 and "source" in proc.stderr
    assert not (tmp_path / "s2").exists()


def test_out_of_tree_files_refused_inside_this_repo(world):
    inside = SCRIPTS.parent / ".architect-export-test-deny.txt"
    try:
        inside.write_text("carol\n")
        proc = world.run("--deny-file", inside)
        assert proc.returncode == 2 and "inside this repository" in proc.stderr
    finally:
        inside.unlink()
    proc = world.run("--staging", SCRIPTS.parent / ".architect-export-test-staging")
    assert proc.returncode == 2 and "inside this repository" in proc.stderr
    assert not (SCRIPTS.parent / ".architect-export-test-staging").exists()


def test_committed_manifest_parses_and_is_clean():
    sys.path.insert(0, str(SCRIPTS))
    import architect_export as tool
    import check_content

    text = (SCRIPTS / "architect_export_manifest.txt").read_text()
    rows = tool.parse_manifest(text)
    assert rows, "the starter manifest lists nothing"
    rules = check_content.load_leak_rules(SCRIPTS / "architect_leak_patterns.txt", deny="")
    assert check_content.leak_findings([("manifest", text.encode())], rules) == []
