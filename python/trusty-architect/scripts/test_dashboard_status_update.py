"""Dashboard Status Update button (the owner 2026-10-09 14:44Z): the write the button makes, the disabled
pending state, the pulse that fires on status_at only, and the fleet-poll alert once per request.
Page logic runs under node (skipped when node is absent), checked on both HTML files."""
import importlib.util
import json
import os
import re
import shutil
import subprocess
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
PAGES = [os.path.join(ROOT, p) for p in ("templates/dashboard/index.html",)]


def run(page, body):
    src = open(page, encoding="utf-8").read()
    m = re.search(r"/\* BEGIN progressCore \*/(.*?)/\* END progressCore \*/", src, re.S)
    assert m
    r = subprocess.run(["node", "-e", m.group(1) + "\n" + body], capture_output=True, text=True, timeout=30)
    assert r.returncode == 0, r.stderr
    return json.loads(r.stdout)


@unittest.skipUnless(shutil.which("node"), "node not installed")
class TestStatusUpdatePage(unittest.TestCase):
    def each(self, body):
        for page in PAGES:
            with self.subTest(page=os.path.basename(os.path.dirname(page))):
                yield run(page, body)

    def test_patch_shape_sets_request_and_updated_only(self):
        for r in self.each('console.log(JSON.stringify(statusRequestPatch("Owner", "2026-10-09T14:50:00.000Z")));'):
            self.assertEqual(r, {"status_request": {"at": "2026-10-09T14:50:00.000Z", "by": "Owner"}, "updated": "2026-10-09T14:50:00.000Z"})
            self.assertNotIn("status", r)

    def test_button_disabled_while_request_pending(self):
        body = """
        const at = "2026-10-09T14:50:00.000Z", d = new Date(at), p = (n) => (n < 10 ? "0" : "") + n;
        console.log(JSON.stringify([statusButtonState({}), statusButtonState({ status_request: null }),
          statusButtonState({ status_request: { at: at, by: "Owner" } }), "Requested " + p(d.getHours()) + ":" + p(d.getMinutes())]));"""
        for r in self.each(body):
            self.assertEqual(r[0], {"label": "Status Update", "disabled": False})
            self.assertEqual(r[1], r[0])
            self.assertEqual(r[2], {"label": r[3], "disabled": True})

    def test_pulse_on_status_at_change_only(self):
        body = """
        const a = { id: "T01", title: "x", status: "open" };
        const base = changedDocs(null, [a]).next;
        const req = changedDocs(base, [Object.assign({}, a, { status_request: { at: "t1", by: "Owner" }, updated: "t1" })]);
        const answered = changedDocs(req.next, [Object.assign({}, a, { status_note: "reported: done", status_at: "t2", status_request: null, updated: "t2" })]);
        const first = changedDocs(null, [a]);
        const other = changedDocs(base, [Object.assign({}, a, { title: "y" })]);
        console.log(JSON.stringify([req.changed, answered.changed, first.changed, other.changed]));"""
        for r in self.each(body):
            self.assertEqual(r, [[], ["T01"], [], ["T01"]])   # a request alone does not pulse; the answer does; first load never


def load_fp():
    spec = importlib.util.spec_from_file_location("fleet_poll", os.path.join(HERE, "fleet-poll.py"))
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    return m


def put(root, item_id, **data):
    d = root / "items"
    d.mkdir(exist_ok=True)
    (d / f"{item_id}.json").write_text(json.dumps({"version": 1, "data": data}))


def test_alert_once_per_request_and_new_request_alerts_again(tmp_path):
    fp = load_fp()
    put(tmp_path, "T05", status="open", title="Ship it", status_request={"at": "2026-10-09T14:50:00.000Z", "by": "Owner"})
    st = {}
    new, _ = fp.dedup(st, fp.owner_status_alerts(1000, str(tmp_path)), 1000)
    assert len(new) == 1 and new[0]["kind"] == "status_request"
    assert new[0]["fingerprint"] == "T05@2026-10-09T14:50:00.000Z"
    assert new[0]["display"] == "[dashboard]: status update requested: T05 Ship it"
    again, _ = fp.dedup(st, fp.owner_status_alerts(1060, str(tmp_path)), 1060)
    assert again == []
    put(tmp_path, "T05", status="open", title="Ship it", status_request={"at": "2026-10-09T15:10:00.000Z", "by": "Owner"})
    third, _ = fp.dedup(st, fp.owner_status_alerts(1120, str(tmp_path)), 1120)
    assert len(third) == 1


def test_answered_request_is_not_pending(tmp_path):
    fp = load_fp()
    put(tmp_path, "T05", status="open", title="Ship it", status_request=None, status_note="verified: x", status_at="2026-10-09T15:00:00Z")
    assert fp.owner_status_alerts(1000, str(tmp_path)) == []
    spec = importlib.util.spec_from_file_location("pending_answers", os.path.join(HERE, "pending-answers.py"))
    pa = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(pa)
    assert pa.main(["--root", str(tmp_path)]) == 0


def test_pending_answers_lists_open_status_request(tmp_path, capsys):
    spec = importlib.util.spec_from_file_location("pending_answers", os.path.join(HERE, "pending-answers.py"))
    pa = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(pa)
    put(tmp_path, "T05", status="open", title="Ship it", status_request={"at": "2026-10-09T14:50:00.000Z", "by": "Owner"})
    assert pa.main(["--root", str(tmp_path)]) == 1
    assert capsys.readouterr().out.startswith("T05\tstatus_request\t2026-10-09T14:50:00.000Z\t")
