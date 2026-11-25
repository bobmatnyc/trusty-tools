#!/usr/bin/env python3
"""architect_bundle_meta.py — the Architect bundle's provenance stamp and seed history (#10571).

Why: an installed Architect must say which prototype commit and which content its
  scripts came from, and `tm fleet init` must tell an unedited old copy (safe to
  refresh) from an owner's edit (kept). Both need data generated from the tree,
  checked so it cannot go stale silently.
What: two subcommands, each with --check (exit 1 and name what is stale; write nothing).
  provenance [--commit SHA]  writes python/trusty-architect/provenance.json:
      {schema, prototype_commit, export_manifest, export_manifest_sha256, files}
      where files maps every shipped pair source (scripts/check_architect_subproject.sh
      --list-pairs, provenance.json itself excluded) to its sha256. --commit names the
      prototype commit the content was exported from; without it the recorded one stays.
      `fixtures` records the sha256 of each test fixture in FIXTURES (#10579).
  history [--from-git] [--from-repo PATH [--paths DEST ...]]
                             writes crates/trusty-mpm/src/bin/tm/commands/fleet/seed_history.tsv:
      one "DEST<TAB>SHA256" line per byte-version tm fleet init has shipped for each
      embedded seed row of the table files in TABLES. Without --from-git the existing lines
      are kept and the current shipped bytes are added; --from-git also adds every version of
      each asset in git history. --from-repo adds, for every seeded scripts/ DEST, one line
      per distinct blob that DEST has had in any commit of the git repo at PATH (the live
      Architect checkout, read with git log/show only); --paths limits it to the listed
      seeded script DESTs. --check fails when a current version is missing.
Exit: 0 written or current; 1 stale (--check); 2 bad input.
Test: scripts/tests/test_architect_bundle_meta.py
"""

import argparse
import hashlib
import json
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SUB = ROOT / "python" / "trusty-architect"
PROVENANCE = SUB / "provenance.json"
MANIFEST = "scripts/architect_export_manifest.txt"
ASSETS = "crates/trusty-mpm/src/assets/architect"
FLEET = ROOT / "crates" / "trusty-mpm" / "src" / "bin" / "tm" / "commands" / "fleet"
# #10571: the seed table spans these files; seed_rows() reads every one, in write order.
TABLES = (FLEET / "seed_table.rs", FLEET / "seed_table_scripts.rs")
HISTORY = FLEET / "seed_history.tsv"
# Test fixtures the bundle's behaviour is checked against (#10579): recorded, not shipped.
FIXTURES = ("crates/trusty-mpm/src/bin/tm/commands/fleet/il_init_sample_state.md",)
ROW = re.compile(r'(embedded|template|schema|page|script)!\(\s*"([^"]+)"(?:\s*,\s*"([^"]+)")?')


class Bad(Exception):
    """Input the tool will not act on (exit 2)."""


def sha(data):
    return hashlib.sha256(data).hexdigest()


def pair_sources(root):
    """Shipped pair sources under the sub-project, from the gate's own pair table."""
    out = subprocess.run([str(root / "scripts" / "check_architect_subproject.sh"), "--root", str(root), "--list-pairs"],
                         capture_output=True, text=True)
    if out.returncode != 0:
        raise Bad(f"--list-pairs failed: {out.stderr.strip()}")
    return sorted(p.split("|", 1)[0] for p in out.stdout.split() if p and p.split("|", 1)[0] != "provenance.json")


def build_provenance(root, commit):
    sub = root / "python" / "trusty-architect"
    files = {}
    for rel in pair_sources(root):
        path = sub / rel
        if not path.is_file():
            raise Bad(f"pair source missing: {rel}")
        files[rel] = sha(path.read_bytes())
    fixtures = {Path(rel).name: sha((root / rel).read_bytes()) for rel in FIXTURES if (root / rel).is_file()}   # by name: the sub-project cites no main-tree path
    return {"schema": 1, "prototype_commit": commit, "export_manifest": MANIFEST,
            "export_manifest_sha256": sha((root / MANIFEST).read_bytes()), "files": files,
            "fixtures": fixtures}


def provenance(args, root=ROOT):
    path = root / "python" / "trusty-architect" / "provenance.json"
    old = json.loads(path.read_text(encoding="utf-8")) if path.is_file() else {}
    commit = args.commit or old.get("prototype_commit")
    if not commit or not re.fullmatch(r"[0-9a-f]{7,40}", commit):
        raise Bad("provenance needs --commit <prototype sha> (7-40 lowercase hex)")
    new = build_provenance(root, commit)
    text = json.dumps(new, indent=2, sort_keys=True) + "\n"
    if args.check:
        if old == new:
            return 0
        stale = sorted(k for k in set(new["files"]) | set(old.get("files", {}))
                       if new["files"].get(k) != old.get("files", {}).get(k))
        for k in stale:
            print(f"STALE provenance: {k}", file=sys.stderr)
        if old.get("export_manifest_sha256") != new["export_manifest_sha256"]:
            print("STALE provenance: export manifest hash", file=sys.stderr)
        print("run: scripts/architect_bundle_meta.py provenance", file=sys.stderr)
        return 1
    path.write_text(text, encoding="utf-8")
    print(f"provenance: prototype {commit}, {len(new['files'])} file(s)")
    return 0


def seed_rows(root):
    """(dest, asset path under the repo) for every embedded seed row, in table order."""
    text = "".join((root / t.relative_to(ROOT)).read_text(encoding="utf-8") for t in TABLES)
    rows = []
    for kind, a, b in ROW.findall(text):
        if kind == "script":   # #10571: script!("name", exec) seeds scripts/<name>
            rows.append((f"scripts/{a}", f"{ASSETS}/scripts/{a}"))
        elif kind == "embedded":
            rows.append((a, f"{ASSETS}/{b}"))
        elif kind == "template":
            rows.append((a, f"{ASSETS}/templates/{a}"))
        elif kind == "schema":
            rows.append((f"schemas/dashboard/{a}", f"{ASSETS}/schemas/dashboard/{a}"))
        else:
            rows.append((f"templates/dashboard/{a}", f"{ASSETS}/dashboard/{a}"))
    if not rows:
        raise Bad(f"no seed rows found in {', '.join(t.name for t in TABLES)}")
    return rows


def git_versions(root, rel):
    """sha256 of every committed version of `rel` (empty when git is unavailable)."""
    log = subprocess.run(["git", "-C", str(root), "log", "--format=%H", "--", rel], capture_output=True, text=True)
    out = set()
    for commit in log.stdout.split():
        blob = subprocess.run(["git", "-C", str(root), "show", f"{commit}:{rel}"], capture_output=True)
        if blob.returncode == 0:
            out.add(sha(blob.stdout))
    return out


def repo_versions(repo, dest):
    """sha256 of every distinct blob `dest` has had in any commit of the git repo `repo`.

    Why: the live Architect checkout is where the prototype scripts evolve; each version it
      has held is a shipped copy `tm fleet init` may meet and must refresh (#10571).
    What: one `git log --all -m --raw` per DEST for the blob ids, then `git show <blob>`
      per distinct blob. Read-only: no checkout and nothing run from `repo`.
    Test: test_history_from_repo_adds_every_live_script_version
    """
    log = subprocess.run(["git", "-C", str(repo), "log", "--all", "-m", "--raw", "--no-abbrev", "--format=",
                          "--", dest], capture_output=True, text=True)
    if log.returncode != 0:
        raise Bad(f"git log in {repo} failed: {log.stderr.strip()}")
    blobs = set()
    for line in log.stdout.splitlines():
        if line.startswith(":"):
            blob = line.split("\t", 1)[0].split()[3]
            if blob.strip("0"):   # all zeros: the path was deleted in that commit
                blobs.add(blob)
    out = set()
    for blob in sorted(blobs):
        show = subprocess.run(["git", "-C", str(repo), "show", "--no-textconv", blob], capture_output=True)
        if show.returncode != 0:
            raise Bad(f"git show {blob} in {repo} failed")
        out.add(sha(show.stdout))
    return out


def repo_dests(args, rows):
    """The seeded scripts/ DESTs `--from-repo` reads: all of them, or the `--paths` subset."""
    scripts = [d for d, _ in rows if d.startswith("scripts/")]
    if not args.paths:
        return scripts
    unknown = sorted(set(args.paths) - set(scripts))
    if unknown:
        raise Bad(f"--paths names no seeded script: {', '.join(unknown)}")
    return [d for d in scripts if d in args.paths]


def history(args, root=ROOT):
    path = root / HISTORY.relative_to(ROOT)
    have = set()
    if path.is_file():
        for num, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
            if not line or line.startswith("#"):
                continue
            parts = line.split("\t")
            if len(parts) != 2 or not re.fullmatch(r"[0-9a-f]{64}", parts[1]):
                raise Bad(f"{path.name}:{num}: expected DEST<TAB>SHA256")
            have.add((parts[0], parts[1]))
    want = set(have)
    missing = []
    rows = seed_rows(root)
    # #10571: every version the live Architect repo has held becomes a row.
    if args.paths and not args.from_repo:
        raise Bad("--paths needs --from-repo")
    if args.from_repo:
        repo = Path(args.from_repo)
        if subprocess.run(["git", "-C", str(repo), "rev-list", "-n", "1", "--all"],
                          capture_output=True).returncode != 0:
            raise Bad(f"--from-repo: not a git repository: {repo}")
        for dest in repo_dests(args, rows):
            want |= {(dest, h) for h in repo_versions(repo, dest)}
    for dest, asset in rows:
        cur = (dest, sha((root / asset).read_bytes()))
        if cur not in have:
            missing.append(dest)
        want.add(cur)
        if args.from_git:
            want |= {(dest, h) for h in git_versions(root, asset)}
    if args.check:
        for dest in missing:
            print(f"STALE seed history: {dest} (current bytes not recorded)", file=sys.stderr)
        if missing:
            print("run: scripts/architect_bundle_meta.py history", file=sys.stderr)
        return 1 if missing else 0
    head = ("# seed_history.tsv: every byte-version `tm fleet init` has shipped per seed row (#10571).\n"
            "# A live file whose sha256 is listed for its DEST is an unedited shipped copy and is refreshed;\n"
            "# any other bytes are an edit and are kept. Generated: scripts/architect_bundle_meta.py history.\n")
    path.write_text(head + "".join(f"{d}\t{h}\n" for d, h in sorted(want)), encoding="utf-8")
    print(f"history: {len(want)} line(s), {len(want) - len(have)} new")
    return 0


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("provenance")
    p.add_argument("--commit", default="")
    p.add_argument("--check", action="store_true")
    h = sub.add_parser("history")
    h.add_argument("--from-git", action="store_true")
    h.add_argument("--from-repo", default="", metavar="PATH")
    h.add_argument("--paths", nargs="+", default=[], metavar="DEST")
    h.add_argument("--check", action="store_true")
    args = ap.parse_args(argv)
    try:
        return provenance(args) if args.cmd == "provenance" else history(args)
    except (Bad, OSError, ValueError) as err:
        print(f"FAIL {err}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
