"""architect-idle.py: transcript tail decides idle; tool_use tail is busy."""
import importlib.util
import json
import os

HERE = os.path.dirname(os.path.abspath(__file__))
# #10161: the shipped layout keeps the modules in ../scripts, the tests in tests/.
SCRIPTS = os.path.join(os.path.dirname(HERE), "scripts")
_spec = importlib.util.spec_from_file_location("architect_idle", os.path.join(SCRIPTS, "architect-idle.py"))
ai = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(ai)


def _a(stop, content):
    return json.dumps({"type": "assistant", "message": {"role": "assistant", "stop_reason": stop, "content": content}})


USER = json.dumps({"type": "user", "message": {"role": "user", "content": "hi"}})
SYS = json.dumps({"type": "system", "content": "x"})
TOOL = [{"type": "tool_use", "id": "t1", "name": "Bash", "input": {}}]
TEXT = [{"type": "text", "text": "done"}]


def _write(tmp_path, lines, age):
    p = tmp_path / "s.jsonl"
    p.write_text("\n".join(lines) + "\n")
    t = p.stat().st_mtime
    return str(p), t + age


def test_end_turn_tail_is_idle(tmp_path):
    p, now = _write(tmp_path, [USER, _a("end_turn", TEXT), SYS], 60)
    assert ai.check(p, now)[0] == 0


def test_tool_use_tail_is_busy(tmp_path):
    p, now = _write(tmp_path, [USER, _a("tool_use", TOOL)], 60)
    assert ai.check(p, now)[0] == 1


def test_fresh_mtime_is_busy_even_after_end_turn(tmp_path):
    p, now = _write(tmp_path, [USER, _a("end_turn", TEXT)], 5)
    assert ai.check(p, now)[0] == 1


def test_trailing_user_record_is_busy(tmp_path):
    p, now = _write(tmp_path, [_a("end_turn", TEXT), USER], 60)
    assert ai.check(p, now)[0] == 1


def test_no_records_is_unknown(tmp_path):
    p, now = _write(tmp_path, [SYS], 60)
    assert ai.check(p, now)[0] == 2
