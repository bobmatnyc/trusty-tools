"""Tests for scripts/self-ctx.py: tail-only transcript reading, no tmux involved."""
import importlib.util
import json
import os

SCRIPTS = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "scripts")


def _load(name, file):
    spec = importlib.util.spec_from_file_location(name, os.path.join(SCRIPTS, file))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


sc = _load("self_ctx", "self-ctx.py")


def _usage_line(usage, msg_type="assistant", entrypoint="cli"):
    return json.dumps({"type": msg_type, "message": {"usage": usage},
                       "entrypoint": entrypoint}) + "\n"


def _write(path, lines, mtime):
    path.write_text(lines)
    os.utime(path, (mtime, mtime))


# --- newest_transcript: entrypoint filter + mtime ---------------------------

def test_newest_transcript_picks_the_highest_mtime_among_cli_files(tmp_path):
    old = tmp_path / "old.jsonl"
    new = tmp_path / "new.jsonl"
    _write(old, _usage_line({"input_tokens": 1}), 1000)
    _write(new, _usage_line({"input_tokens": 2}), 2000)
    assert sc.newest_transcript(str(tmp_path)) == str(new)


def test_newest_transcript_missing_or_empty_dir_returns_none(tmp_path):
    assert sc.newest_transcript(str(tmp_path / "does-not-exist")) is None
    assert sc.newest_transcript(str(tmp_path)) is None  # exists, no *.jsonl
    (tmp_path / "not-a-transcript.txt").write_text("x")
    assert sc.newest_transcript(str(tmp_path)) is None  # wrong extension


def test_newest_transcript_skips_a_newer_sdk_cli_file_for_an_older_cli_one(tmp_path):
    """The question-collector regression (2026-09-27): a `claude -p --agent`
    run (entrypoint "sdk-cli") lands in the same directory every few minutes
    and can out-mtime the Architect's own interactive session. Its transcript
    must never be picked over an older, genuinely interactive one."""
    collector = tmp_path / "collector.jsonl"
    architect = tmp_path / "architect.jsonl"
    _write(architect, _usage_line({"input_tokens": 999_000}, entrypoint="cli"), 1000)
    _write(collector, _usage_line({"input_tokens": 1}, entrypoint="sdk-cli"), 9000)  # newer
    assert sc.newest_transcript(str(tmp_path)) == str(architect)
    assert sc.self_ctx_tokens(str(tmp_path)) == 999_000


def test_newest_transcript_bounds_the_files_it_examines(tmp_path):
    """Only the `limit` newest-by-mtime files are checked, so a directory
    dominated by high-churn sdk-cli transcripts stays cheap to scan even when
    it means missing an older cli file outside that window."""
    cli = tmp_path / "cli.jsonl"
    _write(cli, _usage_line({"input_tokens": 1}, entrypoint="cli"), 1000)
    for i in range(5):
        _write(tmp_path / f"sdk-{i}.jsonl", _usage_line({"input_tokens": 1},
               entrypoint="sdk-cli"), 2000 + i)
    assert sc.newest_transcript(str(tmp_path), limit=3) is None  # cli.jsonl not in top 3
    assert sc.newest_transcript(str(tmp_path), limit=10) == str(cli)  # in top 10


# --- tail_last_assistant_usage -----------------------------------------------

def test_tail_last_assistant_usage_skips_non_assistant_lines(tmp_path):
    f = tmp_path / "t.jsonl"
    f.write_text(
        _usage_line({"input_tokens": 1}, "user")
        + _usage_line({"input_tokens": 10, "cache_read_input_tokens": 20})
    )
    usage = sc.tail_last_assistant_usage(str(f))
    assert usage == {"input_tokens": 10, "cache_read_input_tokens": 20}


def test_tail_last_assistant_usage_empty_file_returns_none(tmp_path):
    f = tmp_path / "empty.jsonl"
    f.write_text("")
    assert sc.tail_last_assistant_usage(str(f)) is None


def test_tail_last_assistant_usage_grows_window_past_a_small_start(tmp_path):
    """The only usage line sits well outside a tiny first tail window (it is
    the very first line, followed by ~400KB of non-matching filler); the
    reader must grow the window until it covers the whole file rather than
    give up after the first, small read."""
    f = tmp_path / "big.jsonl"
    filler = json.dumps({"type": "user", "message": {"content": "x" * 200}}) + "\n"
    with open(f, "w") as fh:
        fh.write(_usage_line({"input_tokens": 900_000, "cache_read_input_tokens": 0}))
        for _ in range(2000):        # ~400KB of filler after the usage line
            fh.write(filler)
    usage = sc.tail_last_assistant_usage(str(f), start=1024)
    assert usage == {"input_tokens": 900_000, "cache_read_input_tokens": 0}


def test_tail_last_assistant_usage_returns_the_last_one_when_several_exist(tmp_path):
    f = tmp_path / "t.jsonl"
    f.write_text(
        _usage_line({"input_tokens": 1})
        + _usage_line({"input_tokens": 2})
        + _usage_line({"input_tokens": 3})
    )
    assert sc.tail_last_assistant_usage(str(f)) == {"input_tokens": 3}


# --- self_ctx_tokens ----------------------------------------------------------

def test_self_ctx_tokens_sums_the_three_usage_fields(tmp_path):
    f = tmp_path / "t.jsonl"
    f.write_text(_usage_line({"input_tokens": 100, "cache_read_input_tokens": 200,
                              "cache_creation_input_tokens": 300}))
    assert sc.self_ctx_tokens(str(tmp_path)) == 600


def test_self_ctx_tokens_missing_fields_default_to_zero(tmp_path):
    f = tmp_path / "t.jsonl"
    f.write_text(_usage_line({"input_tokens": 5}))
    assert sc.self_ctx_tokens(str(tmp_path)) == 5


def test_self_ctx_tokens_none_when_no_transcript(tmp_path):
    assert sc.self_ctx_tokens(str(tmp_path)) is None


def test_self_ctx_tokens_none_when_no_assistant_usage_line(tmp_path):
    f = tmp_path / "t.jsonl"
    f.write_text(_usage_line({"input_tokens": 1}, "user"))
    assert sc.self_ctx_tokens(str(tmp_path)) is None


def test_self_ctx_tokens_none_when_only_sdk_cli_transcripts_exist(tmp_path):
    f = tmp_path / "collector.jsonl"
    f.write_text(_usage_line({"input_tokens": 999_000}, entrypoint="sdk-cli"))
    assert sc.self_ctx_tokens(str(tmp_path)) is None
