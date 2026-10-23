"""stale-tasks.py: open, unparked T items with no activity inside --hours.

Test: scripts/test_stale_tasks.py
"""
import importlib.util
import json
import os
from datetime import datetime, timezone

HERE = os.path.dirname(os.path.abspath(__file__))
# #10161: the shipped layout keeps the modules in ../scripts, the tests in tests/.
SCRIPTS = os.path.join(os.path.dirname(HERE), "scripts")
_spec = importlib.util.spec_from_file_location("stale_tasks", os.path.join(SCRIPTS, "stale-tasks.py"))
st = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(st)

NOW = datetime(2026, 10, 11, 12, 0, tzinfo=timezone.utc)


def put(root, item_id, **data):
    d = root / "items"
    d.mkdir(exist_ok=True)
    (d / f"{item_id}.json").write_text(json.dumps(dict(data, version=1)))


def test_lists_only_stale_open_unparked_tasks(tmp_path, capsys):
    old = "2026-10-09T00:00:00Z"
    put(tmp_path, "T01", kind="task", status="open", session="s1", title="old", updated=old)
    put(tmp_path, "T02", kind="task", status="open", session="s1", title="fresh update", updated=old,
        updates=[{"at": "2026-10-11T10:00:00Z", "text": "x"}])
    put(tmp_path, "T03", kind="task", status="open", session="s2", title="fresh progress", updated=old,
        progress={"estimated_at": "2026-10-11T08:00:00Z"})
    put(tmp_path, "T04", kind="task", status="open", session="s2", title="fresh updated", updated="2026-10-11T11:00:00Z")
    put(tmp_path, "T05", kind="task", status="open", title="parked", updated=old, parked={"until": None, "choice": "stale"})
    put(tmp_path, "T06", kind="task", status="done", title="done", updated=old)
    put(tmp_path, "D07", kind="question", status="open", title="decision", updated=old)
    put(tmp_path, "T08", kind="task", status="open", session="s3", title="older", updated="2026-10-01T00:00:00Z")
    put(tmp_path, "T09", kind="task", status="open", session="s3", title="no stamps")
    assert [r[1] for r in st.stale(str(tmp_path), 24, NOW)] == ["T09", "T08", "T01"]
    assert [r[1] for r in st.stale(str(tmp_path), 24 * 30, NOW)] == ["T09"]


def test_cli_prints_tab_separated_and_hours_flag(tmp_path, capsys):
    put(tmp_path, "T01", kind="task", status="open", session="s1", title="old", updated="2026-10-09T00:00:00Z")
    assert st.main(["--root", str(tmp_path), "--hours", "1"]) == 0
    assert capsys.readouterr().out == "T01\ts1\t2026-10-09T00:00:00Z\told\n"
    assert st.main(["--root", str(tmp_path / "nope")]) == 2
