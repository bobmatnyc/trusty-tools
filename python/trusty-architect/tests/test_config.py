"""Tests for the import-time configuration that replaced the prototype's
operator-specific constants (#8436): session names, inbox and quiet-list paths,
and the self-context transcript directory. Pure; no tmux involved."""
import importlib.util
import os

SCRIPTS = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "scripts")
ROOT = os.path.dirname(SCRIPTS)
ENV = ("ARCHITECT_SESSION", "ARCHITECT_POLL_SESSION", "ARCHITECT_INBOX_DIR",
       "ARCHITECT_QUIET_SESSIONS_FILE", "ARCHITECT_PROJECT_DIR", "CLAUDE_CONFIG_DIR",
       "SELF_CTX_DIR")


def _fresh(monkeypatch, name, file, **env):
    """Load `file` as a new module after setting exactly `env` (others unset)."""
    for key in ENV:
        monkeypatch.delenv(key, raising=False)
    for key, value in env.items():
        monkeypatch.setenv(key, value)
    spec = importlib.util.spec_from_file_location(name, os.path.join(SCRIPTS, file))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def test_poller_defaults_name_the_architect_and_stay_inside_the_checkout(monkeypatch):
    fp = _fresh(monkeypatch, "fleet_poll_defaults", "fleet-poll.py")
    assert fp.ARCHITECT == "tm-architect"
    assert fp.SKIP_SESSIONS == {"tm-architect", "tm-architect-poll"}
    assert fp.INBOX == os.path.join(ROOT, "inbox")
    assert fp.QUIET_SESSIONS_FILE == os.path.join(ROOT, "quiet-sessions.txt")


def test_poller_session_and_paths_follow_the_environment(monkeypatch, tmp_path):
    fp = _fresh(monkeypatch, "fleet_poll_env", "fleet-poll.py",
                ARCHITECT_SESSION="tm-arch-7", ARCHITECT_INBOX_DIR=str(tmp_path),
                ARCHITECT_QUIET_SESSIONS_FILE=str(tmp_path / "q.txt"))
    assert fp.ARCHITECT == "tm-arch-7"
    assert fp.SKIP_SESSIONS == {"tm-arch-7", "tm-arch-7-poll"}  # poll name derives
    assert fp.ALERTS == str(tmp_path / "alerts.jsonl")
    assert fp.QUIET_SESSIONS_FILE == str(tmp_path / "q.txt")


def test_project_slug_replaces_every_non_alphanumeric_character():
    sc = _load_self_ctx()
    assert sc.project_slug("/srv/u/trusty-mpm-projects/architect") == \
        "-srv-u-trusty-mpm-projects-architect"
    assert sc.project_slug("/a/.claude/x_y") == "-a--claude-x-y"


def test_self_ctx_default_dir_uses_documented_defaults_when_unset(monkeypatch):
    sc = _load_self_ctx()
    home = os.path.expanduser("~")
    expected = os.path.join(home, ".trusty-tools/trusty-mpm/claude-config", "projects",
                            sc.project_slug(os.path.join(home, "trusty-mpm-projects/architect")))
    assert sc.default_dir({}) == expected


def test_self_ctx_default_dir_follows_the_environment(tmp_path):
    sc = _load_self_ctx()
    env = {"CLAUDE_CONFIG_DIR": str(tmp_path / "cfg"), "ARCHITECT_PROJECT_DIR": "/srv/arch"}
    assert sc.default_dir(env) == str(tmp_path / "cfg" / "projects" / "-srv-arch")


def _load_self_ctx():
    spec = importlib.util.spec_from_file_location("self_ctx_cfg",
                                                  os.path.join(SCRIPTS, "self-ctx.py"))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod
