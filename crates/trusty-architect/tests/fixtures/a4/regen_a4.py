"""Regenerate the A4 verb goldens from the Architect prototype (#9822).

Why: #9822 swaps the prototype's scripts for `tm architect` verbs, so each
verb's stdout and exit status must match what the Python prints for the same
input. The goldens must come from running the Python, never from the Rust.
What: copies the prototype's scripts/ directory into a system tempdir and runs
only that copy (HOME, SUPERVISOR_INBOX_DIR and PATH all point inside the
tempdir; the prototype directory is read once by the copy and never written).
Writes, into this directory:
  classify.jsonl        json.dumps(parse_pane(capture)) per panes/*.ansi, name order
  input_state_cli.txt   stdout of `input-state.py <every target> %99 =tm-gone:`
                        with fake-tmux.sh as `tmux`; its exit status in cli_exit.json
  poll_dry_panes.txt    fleet-poll.py's dry-run pane line for each watched pane of
                        panes/list-panes.txt (cycle()'s f-string, verbatim)
  poll_args.jsonl       [argv, exit status] of fleet-poll.py for argv argparse refuses
  cli_exit.json         {"input_state": <exit status>}
Test: tests/verbs_parity.rs (trusty-architect) and tests/tm_architect_verbs.rs
(trusty-mpm) read every file written here.

Regenerate (from the repository root):
    python3 -B -I crates/trusty-architect/tests/fixtures/a4/regen_a4.py <prototype-dir>
where <prototype-dir> is the Architect prototype's scripts/ directory.
"""
import importlib.util
import json
import os
import shutil
import subprocess
import sys
import tempfile

sys.dont_write_bytecode = True
HERE = os.path.dirname(os.path.abspath(__file__))
PANES = os.path.join(HERE, "panes")
NOW = 1760290860.0  # 2025-10-12T17:41:00Z, a fixed clock for evaluate()
if len(sys.argv) != 2:
    sys.exit("usage: regen_a4.py <prototype-dir>  (the Architect prototype's scripts/ directory)")
PROTOTYPE = os.path.abspath(sys.argv[1])


def write(name, text):
    with open(os.path.join(HERE, name), "w", encoding="utf-8", newline="\n") as f:
        f.write(text)


def main():
    names = sorted(n for n in os.listdir(PANES) if n.endswith(".ansi"))
    with tempfile.TemporaryDirectory() as tmp:
        scripts = os.path.join(tmp, "scripts")
        shutil.copytree(PROTOTYPE, scripts,
                        ignore=shutil.ignore_patterns("__pycache__", ".pytest_cache"))
        bin_dir = os.path.join(tmp, "bin")
        os.makedirs(bin_dir)
        shutil.copy(os.path.join(HERE, "fake-tmux.sh"), os.path.join(bin_dir, "tmux"))
        os.chmod(os.path.join(bin_dir, "tmux"), 0o755)
        env = {"HOME": os.path.join(tmp, "home"), "PATH": bin_dir + ":/usr/bin:/bin",
               "SUPERVISOR_INBOX_DIR": os.path.join(tmp, "inbox"),
               "SESSIONS_DIR": os.path.join(tmp, "sessions"),
               "TRANSCRIPT_BASE": os.path.join(tmp, "projects"),
               "FAKE_TMUX_PANES": PANES, "LANG": "C.UTF-8", "PYTHONIOENCODING": "utf-8"}
        os.environ.clear()
        os.environ.update(env)

        spec = importlib.util.spec_from_file_location("fleet_poll", os.path.join(scripts, "fleet-poll.py"))
        fp = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(fp)

        rows, captures = [], {}
        for name in names:
            with open(os.path.join(PANES, name), encoding="utf-8", newline="") as f:
                captures[name] = f.read()
            rows.append(json.dumps(fp.parse_pane(captures[name])) + "\n")
        write("classify.jsonl", "".join(rows))

        with open(os.path.join(PANES, "targets.tsv"), encoding="utf-8") as f:
            targets = [line.split("\t", 1)[0] for line in f.read().splitlines() if line]
        cli = subprocess.run(
            [sys.executable, "-B", "-I", os.path.join(scripts, "input-state.py"),
             *targets, "%99", "=tm-gone:"],
            env=env, capture_output=True, text=True, timeout=60)
        write("input_state_cli.txt", cli.stdout)
        write("cli_exit.json", json.dumps({"input_state": cli.returncode}) + "\n")

        lines = []
        with open(os.path.join(PANES, "list-panes.txt"), encoding="utf-8") as f:
            listing = [line.split("\t") for line in f.read().splitlines() if line]
        by_pane = {t: n for t, n in (line.split("\t") for line in
                   open(os.path.join(PANES, "targets.tsv"), encoding="utf-8").read().splitlines())}
        for s, pid, cmd, *_ in listing:
            if not s.startswith("tm-") or s in fp.SKIP_SESSIONS or cmd in fp.SHELLS:
                continue
            info = fp.parse_pane(captures[by_pane[pid]])
            found = fp.evaluate(info, {}, NOW, s, pid)
            p = {"cmd": cmd}
            # cycle()'s dry-run line, verbatim.
            lines.append(f"  {s} {pid} cmd={p['cmd']} box={info['box']} agents={info['agents']} "
                         f"busy={info['busy']} dialog={bool(info['dialog'])} ctx={info['ctx']} "
                         f"plan={info['plan5h']}/{info['planwk']} q={bool(info['question'])} "
                         f"alerts={[a['kind'] for a in found] or '-'}\n")
        write("poll_dry_panes.txt", "".join(lines))

        args_rows = []
        for argv in (["--interval", "x"], ["--interval"], ["--bogus"], ["--once", "extra"],
                     ["--interval", "1.5"]):
            r = subprocess.run([sys.executable, "-B", "-I", os.path.join(scripts, "fleet-poll.py"), *argv],
                               env=env, capture_output=True, text=True, timeout=60)
            args_rows.append(json.dumps([argv, r.returncode]) + "\n")
        write("poll_args.jsonl", "".join(args_rows))


main()
