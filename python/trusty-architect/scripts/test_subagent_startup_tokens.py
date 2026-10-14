"""subagent-startup-tokens.py on a synthetic transcript tree in a temp dir.

Run: python3 -m unittest scripts/test_subagent_startup_tokens.py
"""
import importlib.util
import json
import os
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location(
    "sst", os.path.join(HERE, "subagent-startup-tokens.py"))
sst = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sst)


def turn(i, c, r):
    return {"type": "assistant", "message": {"usage": {
        "input_tokens": i, "cache_creation_input_tokens": c, "cache_read_input_tokens": r}}}


def write(path, recs, meta=None):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as fh:
        fh.write("not json\n")
        for r in recs:
            fh.write(json.dumps(r) + "\n")
    if meta is not None:
        with open(path[:-6] + ".meta.json", "w") as fh:
            json.dump(meta, fh)


class Case(unittest.TestCase):
    def test_first_turn_per_type_and_unknown(self):
        with tempfile.TemporaryDirectory() as d:
            sub = os.path.join(d, "sess", "subagents")
            user = {"type": "user", "message": {"content": "hi"}}
            write(os.path.join(sub, "agent-a.jsonl"),
                  [user, turn(1, 100, 900), turn(5, 5, 5)], {"agentType": "research"})
            write(os.path.join(sub, "agent-b.jsonl"),
                  [turn(2, 0, 298)], {"agentType": "research"})
            write(os.path.join(sub, "agent-c.jsonl"), [turn(7, 0, 0)])
            data = sst.collect(d)
            self.assertEqual(sorted(data["research"]), [300, 1001])
            self.assertEqual(data[sst.UNKNOWN], [7])
            self.assertEqual(sst.main(["x", d]), 0)


if __name__ == "__main__":
    unittest.main()
