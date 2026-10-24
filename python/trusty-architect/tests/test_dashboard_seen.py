"""Dashboard: a decision is marked seen only when the owner expands it himself (owner 2026-10-09 03:39Z).
Runs the page's attention helpers under node (skipped when node is absent)."""
import json
import os
import re
import shutil
import subprocess
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PAGES = [os.path.join(ROOT, p) for p in ("templates/dashboard/index.html",)]


def run(body):
    out = None
    for page in PAGES:
        with open(page, encoding="utf-8") as f:
            res = run_page(f.read(), body)
        assert out is None or out == res, "pages differ: %r vs %r" % (out, res)
        out = res
    return out


def run_page(src, body):
    ask = re.search(r"/\* BEGIN askKind \*/(.*?)/\* END askKind \*/", src, re.S).group(1)
    attn = re.search(r"/\* BEGIN attention \*/(.*?)/\* END attention \*/", src, re.S).group(1)
    # The page ships with SEEN_ENABLED = false (owner, 2026-10-19); these helper tests exercise the feature as if it were on.
    stub = "const SEEN_ENABLED = true; const painted = []; const $ = () => null; const isParked = () => false; function noteFlash() {}\n" \
        "const document = { body: { classList: { toggle() {}, remove() {}, add() {} }, offsetWidth: 0 } };\n"
    js = ask + stub + attn + "\n" + body
    r = subprocess.run(["node", "-e", js], capture_output=True, text=True, timeout=30)
    assert r.returncode == 0, r.stderr
    return json.loads(r.stdout)


SETUP = """
attn = { pending: new Set(["D1", "D2"]), flash: false };
const row = (open) => ({ open: open, dataset: { pid: "D1" } });
const pend = () => Array.from(attn.pending);
"""


@unittest.skipUnless(shutil.which("node"), "node not installed")
class TestDashboardSeen(unittest.TestCase):
    def test_trusted_expand_clears(self):
        out = run(SETUP + 'expandSeen({ isTrusted: true }, row(false)); console.log(JSON.stringify(pend()));')
        self.assertEqual(out, ["D2"])

    def test_collapse_click_does_not_clear(self):
        out = run(SETUP + 'expandSeen({ isTrusted: true }, row(true)); console.log(JSON.stringify(pend()));')
        self.assertEqual(out, ["D1", "D2"])

    def test_untrusted_click_does_not_clear(self):
        out = run(SETUP + 'expandSeen({ isTrusted: false }, row(false)); expandSeen({}, row(false)); expandSeen(null, row(false)); console.log(JSON.stringify(pend()));')
        self.assertEqual(out, ["D1", "D2"])

    def test_programmatic_open_does_not_clear(self):
        # reopenItem, syncOpen and re-renders set details.open without a click: no handler runs, and a direct call without an event is inert.
        out = run(SETUP + 'seenDecision("D1"); seenDecision("D1", undefined); console.log(JSON.stringify(pend()));')
        self.assertEqual(out, ["D1", "D2"])

    def test_seen_button_needs_trusted_click(self):
        out = run(SETUP + 'seenAll({ isTrusted: false }); const a = pend(); seenAll({ isTrusted: true }); console.log(JSON.stringify([a, pend()]));')
        self.assertEqual(out, [["D1", "D2"], []])

    def test_first_load_and_changed_unchanged(self):
        body = """
        const docs = [{ id: "D1", status: "open", kind: "question" }, { id: "D2", status: "answered", kind: "question" }];
        const first = attentionUpdate({ pending: new Set() }, docs, [], true);
        const chg = attentionUpdate({ pending: new Set() }, docs, ["D1", "D2"], false);
        console.log(JSON.stringify([Array.from(first.pending), first.flash, Array.from(chg.pending), chg.flash]));"""
        out = run(body)
        self.assertEqual(out, [["D1"], False, ["D1"], True])

    def test_seen_survives_reload_until_changed(self):
        # Reading (a): a reload re-flagged every decision the owner had already expanded.
        body = """
        const docs = [{ id: "D1", status: "open", kind: "question", updated: "t1" }, { id: "D2", status: "open", kind: "question", updated: "t2" }];
        const store = {};
        global.localStorage = { getItem: (k) => store[k] || null, setItem: (k, v) => { store[k] = v; } };
        attn = attentionUpdate({ pending: new Set() }, docs, [], true, seenLoad());
        const fresh = Array.from(attn.pending);
        seenDecision("D1", { isTrusted: true });
        attn = attentionUpdate({ pending: new Set() }, docs, [], true, seenLoad());
        const reload = Array.from(attn.pending);
        const edited = [{ id: "D1", status: "open", kind: "question", title: "new title", updated: "t9" }, docs[1]];
        attn = attentionUpdate({ pending: new Set() }, edited, [], true, seenLoad());
        console.log(JSON.stringify([fresh, reload, Array.from(attn.pending)]));"""
        out = run(body)
        self.assertEqual(out, [["D1", "D2"], ["D2"], ["D1", "D2"]])

    def test_note_edit_keeps_seen_but_content_edit_reflags(self):
        # owner 2026-10-09 third report: an Architect edit of answer_note bumped `updated` and re-flagged seen decisions.
        body = """
        const base = { id: "D1", status: "open", kind: "question", title: "T", options: [{ label: "a" }], updated: "t1" };
        const store = {};
        global.localStorage = { getItem: (k) => store[k] || null, setItem: (k, v) => { store[k] = v; } };
        attn = attentionUpdate({ pending: new Set() }, [base], [], true, seenLoad());
        seenDecision("D1", { isTrusted: true });
        const noted = Object.assign({}, base, { updated: "t2", answer_note: "progress" });
        const reload = attentionUpdate({ pending: new Set() }, [noted], [], true, seenLoad());
        const live = attentionUpdate(attn, [noted], ["D1"], false);
        const retitled = attentionUpdate(attn, [Object.assign({}, noted, { title: "T2" })], ["D1"], false);
        console.log(JSON.stringify([Array.from(reload.pending), Array.from(live.pending), live.flash, Array.from(retitled.pending), retitled.flash]));"""
        out = run(body)
        self.assertEqual(out, [[], [], False, ["D1"], True])

    def test_seen_button_marks_only_visible_decisions(self):
        # owner 2026-10-09: the button marked every pending decision, including ones he had not been shown.
        out = run(SETUP + 'seenAll({ isTrusted: true }, (id) => id === "D1"); console.log(JSON.stringify(pend()));')
        self.assertEqual(out, ["D2"])

    def test_seen_on_another_origin_stays_seen(self):
        # owner 2026-10-09 15:37Z: Seen on localhost did not carry to the tailnet origin (separate localStorage). The server record does.
        body = """
        const docs = [{ id: "D1", status: "open", kind: "question", title: "T" }, { id: "D2", status: "open", kind: "question", title: "U" }];
        const store = {};
        global.localStorage = { getItem: (k) => store[k] || null, setItem: (k, v) => { store[k] = v; } };
        const sig = attentionUpdate({ pending: new Set() }, docs, [], true, {}).sigs;
        seenServer = { D1: sig.D1, D2: "stale" };
        const first = attentionUpdate({ pending: new Set() }, docs, [], true, seenMerged());
        const live = attentionUpdate({ pending: new Set(["D1", "D2"]), sigs: first.sigs }, docs, ["D1"], false, seenMerged());
        console.log(JSON.stringify([Array.from(first.pending), Array.from(live.pending)]));"""
        self.assertEqual(run(body), [["D2"], ["D2"]])

    def test_seen_is_persisted_to_the_server(self):
        body = """
        const store = {}; const sent = [];
        global.localStorage = { getItem: (k) => store[k] || null, setItem: (k, v) => { store[k] = v; } };
        attn = attentionUpdate({ pending: new Set() }, [{ id: "D1", status: "open", kind: "question", title: "T" }], [], true, {});
        seenPersist = (add) => sent.push(add);
        seenDecision("D1", { isTrusted: true });
        console.log(JSON.stringify([sent.length, Object.keys(sent[0]), seenServer.D1 === attn.sigs.D1]));"""
        self.assertEqual(run(body), [1, ["D1"], True])

    def test_unparked_decision_seen_before_stays_seen(self):
        body = """
        const open = { id: "D1", status: "open", kind: "question", title: "T" };
        const parked = Object.assign({}, open, { parked: { until: "2026-10-10T00:00:00Z" } });
        const sig = attentionUpdate({ pending: new Set() }, [open], [], true, {}).sigs.D1;
        const m = { D1: [sig] };
        const a = attentionUpdate({ pending: new Set() }, [open], [], true, m);
        const b = attentionUpdate(a, [parked], ["D1"], false, m);
        const c = attentionUpdate(b, [open], ["D1"], false, m);
        console.log(JSON.stringify([Array.from(a.pending), Array.from(c.pending), c.flash]));"""
        self.assertEqual(run(body), [[], [], False])

    def test_answered_and_done_never_pending(self):
        body = """
        const docs = ["answered", "done", "cancelled", "wont_do"].map((s, i) => ({ id: "D" + i, status: s, kind: "question" }));
        const f = attentionUpdate({ pending: new Set(["D0", "D1", "D2", "D3"]) }, docs, ["D0", "D1", "D2", "D3"], true, {});
        const n = attentionUpdate({ pending: new Set(["D0"]) }, docs, ["D0", "D1"], false, {});
        console.log(JSON.stringify([Array.from(f.pending), Array.from(n.pending), n.flash]));"""
        self.assertEqual(run(body), [[], [], False])

    def test_first_snapshot_waits_for_the_server_record(self):
        body = """
        const docs = [{ id: "D1", status: "open", kind: "question", title: "T" }];
        const store = {}; const tracked = [];
        global.localStorage = { getItem: (k) => store[k] || null, setItem: (k, v) => { store[k] = v; } };
        global.items = docs;
        const sig = attentionUpdate({ pending: new Set() }, docs, [], true, {}).sigs.D1;
        gateAttention(docs, { first: true, changed: [] });
        const before = Array.from(attn.pending);
        seenServer = { D1: sig }; seenOpen();
        console.log(JSON.stringify([before, Array.from(attn.pending)]));"""
        self.assertEqual(run(body), [[], []])


class TestSeenDisabled(unittest.TestCase):
    """The owner switched Seen off on 2026-10-19: the switch is false and gates every seen path."""

    def test_switch_is_off_and_gates_every_seen_path(self):
        for page in PAGES:
            src = open(page, encoding="utf-8").read()
            self.assertIn("const SEEN_ENABLED = false;", src, page)
            for guard in ("if (!SEEN_ENABLED) return {};", "if (!SEEN_ENABLED || !ids.length) return;", "if (!SEEN_ENABLED) return;",
                          "if (SEEN_ENABLED && ownerEvent(ev)", "if (!SEEN_ENABLED || !ownerEvent(ev)) return;", "if (SEEN_ENABLED && row &&",
                          "if (!SEEN_ENABLED || !db || readOnly) return;", "if (SEEN_ENABLED) db.doc(\"meta/seen\")"):
                self.assertIn(guard, src, (page, guard))
            self.assertIn("if (!SEEN_ENABLED) { const sb = document.getElementById(\"attn-seen\"); if (sb) sb.remove(); }", src, page)


if __name__ == "__main__":
    unittest.main()
