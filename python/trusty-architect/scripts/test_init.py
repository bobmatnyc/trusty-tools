"""init.sh in a scratch copy: first run creates, second run keeps, settings merge never overwrites,
the guard lift only on request (#8436, ruling Dca A), and a settings file it cannot parse fails the
run without a write."""
import json
import os
import shutil
import subprocess

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)
LIFT = "TRUSTY_MPM_PM_UNRESTRICTED"


def make_root(tmp_path):
    """An installed Architect tree: this checkout's agents/ and skills/ land in
    .claude/, and the instruction files at the root (#8436 layout)."""
    root = tmp_path / "arch"
    root.mkdir()
    for src, dst in (("scripts", "scripts"), ("templates", "templates"),
                     ("agents", ".claude/agents"), ("skills", ".claude/skills")):
        shutil.copytree(os.path.join(REPO, src), root / dst, ignore=shutil.ignore_patterns("__pycache__"))
    for src, dst in (("templates/AGENTS.md", "AGENTS.md"), ("templates/CLAUDE.md", "CLAUDE.md")):
        shutil.copy(os.path.join(REPO, src), root / dst)
    return root


def run(root, tmp_path, *args, env_extra=None):
    env = dict(os.environ, HOME=str(tmp_path / "home"), TMUX_TMPDIR=str(tmp_path))
    env.pop("ARCHITECT_GUARD_LIFT", None)
    env.pop("TMUX", None)
    env.update(env_extra or {})
    (tmp_path / "home").mkdir(exist_ok=True)
    return subprocess.run([str(root / "scripts" / "init.sh"), *args], cwd=root, env=env,
                          capture_output=True, text=True)


def test_first_run_creates_then_second_run_keeps(tmp_path):
    root = make_root(tmp_path)
    first = run(root, tmp_path)
    assert first.returncode == 0, first.stderr
    for f in ("local/state.md", "local/goals.md", "local/improvements/ledger.jsonl",
              "local/usage/ledger.jsonl", "local/.gitignore"):
        assert (root / f).exists(), f
    for gone in ("owner", "local/user.md", "local/projects", "local/improvements/rules.jsonl"):
        assert not (root / gone).exists(), gone
    s = json.loads((root / ".claude/settings.json").read_text())
    assert LIFT not in s.get("env", {}), s
    assert s["statusLine"]["command"].endswith("scripts/statusline-tee.sh")
    before = (root / ".claude/settings.json").read_text()
    second = run(root, tmp_path)
    assert second.returncode == 0, second.stderr
    assert "created " not in second.stdout.replace("Next", "")
    assert (root / ".claude/settings.json").read_text() == before


def test_guard_lift_only_on_request(tmp_path):
    root = make_root(tmp_path)
    (root / ".claude").mkdir(exist_ok=True)
    (root / ".claude/settings.json").write_text(json.dumps({"env": {"FOO": "x"}, "model": "opus"}))
    out = run(root, tmp_path, "--guard-lift")
    assert out.returncode == 0, out.stderr
    s = json.loads((root / ".claude/settings.json").read_text())
    assert s["env"] == {"FOO": "x", LIFT: "1"} and s["model"] == "opus"

    other = tmp_path / "other"
    other.mkdir()
    root2 = make_root(other)
    out = run(root2, other, env_extra={"ARCHITECT_GUARD_LIFT": "1"})
    assert out.returncode == 0, out.stderr
    assert json.loads((root2 / ".claude/settings.json").read_text())["env"][LIFT] == "1"


def test_unparseable_settings_fail_without_a_write(tmp_path):
    for body in ("{ not json", "[1, 2]"):
        case = tmp_path / str(abs(hash(body)))
        case.mkdir()
        root = make_root(case)
        (root / ".claude").mkdir(exist_ok=True)
        (root / ".claude/settings.json").write_text(body)
        out = run(root, case, "--guard-lift")
        assert out.returncode != 0, out.stdout
        assert "not written" in out.stderr, out.stderr
        assert (root / ".claude/settings.json").read_text() == body
        assert not (root / ".claude/settings.json.tmp").exists()


def test_existing_files_and_statusline_are_not_overwritten(tmp_path):
    root = make_root(tmp_path)
    (root / "local").mkdir()
    (root / "local/state.md").write_text("MINE\n")
    (root / ".claude").mkdir(exist_ok=True)
    custom = {"statusLine": {"type": "command", "command": "/x/mine.sh"}, "model": "opus"}
    (root / ".claude/settings.json").write_text(json.dumps(custom))
    out = run(root, tmp_path)
    assert out.returncode == 0, out.stderr
    assert (root / "local/state.md").read_text() == "MINE\n"
    s = json.loads((root / ".claude/settings.json").read_text())
    assert s["statusLine"]["command"] == "/x/mine.sh" and s["model"] == "opus"
    assert "would set" in out.stdout


def test_doctor_fails_on_missing_piece_and_passes_after_init(tmp_path):
    root = make_root(tmp_path)
    doc = [str(root / "scripts/doctor.sh"), "--no-host"]
    assert subprocess.run(doc, cwd=root, capture_output=True).returncode == 1
    run(root, tmp_path)
    res = subprocess.run(doc, cwd=root, capture_output=True, text=True)
    assert res.returncode == 0, res.stdout
    assert "INFO     guard lift: off" in res.stdout
