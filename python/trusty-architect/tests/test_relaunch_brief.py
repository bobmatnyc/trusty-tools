"""The one relaunch-resume brief: old vs new text, edit-in-place sync, chunked send (the owner, briefs-dedupe; see #10571).

#10571 S2: the pin's old-vs-new byte comparison reads the prototype's git history and its check of the checked-in
local/briefs/relaunch-resume.md reads instance state; neither exists in the bundle, so both stay in the prototype.
Test: tests/test_relaunch_brief.py
"""
import os

import pytest

import relaunch_brief as rb
from test_session_relaunch_il import run

def test_rerun_leaves_unchanged_brief_alone_and_writes_no_new_file(tmp_path):
    r, _, log = run(tmp_path, ["--il", "IL"])
    assert r.returncode == 0, r.stderr + log
    brief = tmp_path / "briefs" / "relaunch-resume.md"
    os.utime(brief, (1, 1))
    assert rb.sync(str(brief)) is False
    assert brief.stat().st_mtime == 1 and os.listdir(tmp_path / "briefs") == ["relaunch-resume.md"]


def test_sync_edits_stale_body_in_place_and_keeps_stamped_header(tmp_path):
    brief = tmp_path / "relaunch-resume.md"
    brief.write_text("<!-- brief-id: MB0042 -->\n# MB0042: PM relaunch resume\n\nold text\n")
    assert rb.sync(str(brief)) is True
    text = brief.read_text()
    assert text.startswith("<!-- brief-id: MB0042 -->\n# MB0042: PM relaunch resume\n")
    assert "old text" not in text and rb.TEMPLATES["datg"] in text and rb.TEMPLATES["dbatg"] in text
    assert rb.sync(str(brief)) is False and os.listdir(tmp_path) == ["relaunch-resume.md"]


def test_render_fills_placeholders_literally_and_rejects_unknown_kind():
    assert "{DIGEST_COMMAND}" not in rb.render("datg", "x {y} & z")
    assert "run x {y} & z;" in rb.render("datg", "x {y} & z")
    with pytest.raises(ValueError):
        rb.render("other", "x")


def test_long_text_is_typed_in_pieces_that_never_start_with_a_dash(tmp_path):
    r, st, log = run(tmp_path, ["--il", "IL"], dbatg=[("u1", ["state.json", "cron-prompts.md"])])
    assert r.returncode == 0, r.stderr + log
    line = st["sent"][-1]
    typed = st["typed"]
    pieces = typed[[t.startswith("[Ann Architect") for t in typed].index(True):]
    assert len(line) > 300 and len(pieces) >= 2 and "".join(pieces) == line
    assert all(len(p) <= 300 and not p.startswith("-") for p in pieces)
