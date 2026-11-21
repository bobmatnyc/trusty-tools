"""Regenerates check_cases.json: `ilio.py check` on blocks a golden step does not cover.

Usage: generate_checks.py <architect scripts dir>. Test: `tests/il_ilio.rs` (`check_matches_python_on_more_blocks`).
"""
import json
import os
import shutil
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE)) + "/"
FROZEN = ROOT + "support/frozen_py.py"
S = sys.argv[1]
work = tempfile.mkdtemp(prefix="il-x-")
proj = work + "/p"
shutil.copytree(HERE + "/base", proj)
os.makedirs(work + "/ilio")
shutil.copy(HERE + "/ilio/r-library.json", work + "/ilio/")
env = dict(os.environ, FROZEN_EPOCH="1795200000", ILIO_PROJECT=proj, ILIO_DIR=work + "/ilio",
           DASHBOARD_ROOT=work + "/dash", ARCHITECT_INBOX_DIR=work + "/inbox")


def blk(body):
    return "```il\n" + body + "\n```\n"


base = "done: x\npct: 1\nnext: y"
cases = {
    "free_task": blk("task: free: nothing\ndone: x\npct: 0\nnext: y"),
    "goal_ignored": blk("goal: whatever\ntask: fx-T01\n" + base),
    "two_tasks_and_open": blk("task: fx-T01 fx-T02\ndone: x\npct: 1\nopen: fx-D02\nnext: y"),
    "stale_open": blk("task: fx-T01\ndone: x\npct: 1\nopen: fx-D77\nnext: y"),
    "lookalike_l": blk("task: fx-T0l\n" + base),
    "lookalike_o_comma": blk("task: fx-T01, fx-D0o\n" + base),
    "remind_form": blk("task: fx-T01\ndone: x\npct: 1\nremind: oops\nnext: y"),
    "remind_every": blk("task: fx-T01\ndone: x\npct: 1\nremind: fx-T01 at=2026-12-01 every=0d \"n\"\nnext: y"),
    "remind_ok": blk("task: fx-T01\ndone: x\npct: 1\nremind: fx-T01 at=2026-12-01 every=2w \"n\"\nnext: y"),
    "remind_long_note": blk("task: fx-T01\ndone: x\npct: 1\nremind: fx-T01 at=2026-12-01 \"" + "n" * 170 + "\"\nnext: y"),
    "arabic_pct": blk("task: fx-T01\ndone: x\npct: ٣\nnext: y"),
    "r_ref_ok": blk("task: fx-T01\ndone: R01-a\npct: 5\nnext: y"),
    "secret_password": blk("task: fx-T01\ndone: x\npct: 5\nnext: password = abcdefgh1"),
    "secret_pem": blk("task: fx-T01\ndone: x\npct: 5\nnext: -----BEGIN RSA PRIVATE KEY-----"),
    "bare_id": blk("task: T01\n" + base),
    "other_code_suffix": blk("task: ar-T01\n" + base),
    "stamp_ok": blk("task: fx-T01@1118\n" + base),
    "new_long_ok": blk("task: fx-T01\ndone: x\npct: 1\nnew: T \"" + "t" * 100 + "\" done_when=\"" + "d" * 200 + "\"\nnext: y"),
    "new_too_long": blk("task: fx-T01\ndone: x\npct: 1\nnew: T \"t\" done_when=\"" + "d" * 1200 + "\"\nnext: y"),
    "tabbed_fence": "```il \t\ntask: fx-T01\n" + base + "\n```\n",
    "two_blocks_last_wins": blk("task: fx-T77\n" + base) + "\n" + blk("task: fx-T01\n" + base),
    "colonless_line": blk("task: fx-T01\n" + base + "\nstray words"),
}
out = {}
for name, text in cases.items():
    r = subprocess.run(["python3", "-I", FROZEN, S, "ilio", "check"], input=text, capture_output=True, text=True, env=env)
    out[name] = [text, r.stdout, r.returncode]
json.dump(out, open(HERE + "/check_cases.json", "w"), ensure_ascii=False, indent=1)
for k, v in out.items():
    print(k, v[2], repr(v[1]))
shutil.rmtree(work)
