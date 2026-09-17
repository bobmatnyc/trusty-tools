"""Run one owned disposable daemon/index; preserve every result and source copy."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import signal
import subprocess
import time
import traceback
from pathlib import Path
from typing import Any
from urllib.error import URLError

from benchmark import evaluate, request, verify_fixture

ROOT = Path(__file__).resolve().parent
REVISION = "90b6aeb944e1d010f3690281ec24efe7b89435c3"


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--words", type=int, choices=(0, 64, 128), required=True)
    parser.add_argument("--window", type=int, choices=(100, 64), required=True)
    parser.add_argument("--split", choices=("tuning", "held_out"), default="tuning")
    parser.add_argument("--port", type=int, default=18871)
    parser.add_argument("--kg-cap", type=int, default=100000)
    parser.add_argument("--name")
    parser.add_argument("--binary", type=Path, default=ROOT / "target/debug/examples/context_experiment_daemon")
    args = parser.parse_args()
    name = args.name or f"c{args.words}w{args.window}-{args.split}"
    if "/" in name or name in (".", ".."):
        raise ValueError("Invalid treatment name")
    treatment = ROOT / "runs" / name
    treatment.mkdir(parents=True, exist_ok=False)
    corpus, data = treatment / "corpus", treatment / "data"
    corpus.mkdir()
    data.mkdir()
    archive = ROOT / "source.tar"
    with archive.open("rb") as stream:
        subprocess.run(["tar", "-xf", "-", "-C", str(corpus)], stdin=stream, check=True)
    omitted_symlinks = []
    for path in corpus.rglob("*"):
        if path.is_symlink():
            omitted_symlinks.append(str(path.relative_to(corpus)))
            path.unlink()
    (corpus / ".trusty-search-test-corpus").touch()
    (data / ".trusty-search-test-daemon").touch()
    base = f"http://127.0.0.1:{args.port}"
    env = dict(os.environ)
    env.update({"TRUSTY_DATA_DIR": str(data), "TRUSTY_DATA_DIR_OVERRIDE": str(data / "shared"),
                "TRUSTY_SEARCH_TEST_CORPUS_ROOT": str(corpus), "TRUSTY_SEARCH_TEST_URL": base,
                "TRUSTY_SEARCH_EXPERIMENT_SOURCE_REVISION": REVISION,
                "TRUSTY_SEARCH_EXPERIMENT_CONTEXT_WORDS": str(args.words),
                "TRUSTY_SEARCH_EXPERIMENT_SUBCHUNK_WINDOW": str(args.window),
                "TRUSTY_NO_AUTO_DISCOVER": "1", "TRUSTY_MAX_RESIDENT_INDEXES": "1",
                "RUST_LOG": "warn", "TRUSTY_MAX_KG_NODES": str(args.kg_cap)})
    metadata: dict[str, Any] = {"name": name, "source_revision": REVISION,
        "binary_sha256": hashlib.sha256(args.binary.read_bytes()).hexdigest(),
        "archive_sha256": hashlib.sha256(archive.read_bytes()).hexdigest(),
        "query_sha256": hashlib.sha256((ROOT / "queries.json").read_bytes()).hexdigest(),
        "words": args.words, "window": args.window, "split": args.split, "url": base,
        "omitted_symlinks": omitted_symlinks, "kg_node_cap": args.kg_cap}
    (treatment / "manifest.json").write_text(json.dumps(metadata, indent=2))
    with (treatment / "daemon.log").open("w") as log:
        process = subprocess.Popen([str(args.binary)], env=env, stdout=log, stderr=subprocess.STDOUT,
                                   cwd=corpus, start_new_session=True)
        (treatment / "pid").write_text(str(process.pid))
        try:
            deadline = time.monotonic() + 90
            while True:
                if process.poll() is not None:
                    raise RuntimeError("Daemon exited; inspect daemon.log")
                try:
                    evidence = request(base, "/experiment/evidence")
                    if Path(evidence["data_dir"]).resolve() != data.resolve():
                        raise ValueError("Port belongs to another daemon")
                    break
                except (URLError, ConnectionError):
                    if time.monotonic() > deadline:
                        raise TimeoutError("Daemon startup")
                    time.sleep(.5)
            started = time.monotonic()
            created = request(base, "/indexes", {"id": "experiment", "root_path": str(corpus),
                               "skip_vector": True, "skip_kg": False})
            if created.get("created") is not True:
                raise ValueError(f"Index was not fresh: {created}")
            request(base, "/indexes/experiment/reindex", {"force": True})
            deadline = time.monotonic() + 1800
            last_print = 0.0
            rss_samples: list[int] = []
            while True:
                if process.poll() is not None:
                    raise RuntimeError("Daemon exited during indexing")
                status = request(base, "/indexes/experiment/status")
                stage_states = {k: v.get("status") for k, v in status.get("stages", {}).items() if isinstance(v, dict)}
                if any(stage_states.get(k) == "failed" for k in ("lexical", "graph")):
                    raise ValueError(f"Index stage failure: {status}")
                try:
                    rss_samples.append(int(subprocess.check_output(["ps", "-o", "rss=", "-p", str(process.pid)], text=True).strip()))
                except (subprocess.CalledProcessError, ValueError):
                    pass
                now = time.monotonic()
                if now - last_print > 15:
                    print(name, round(now - started), status.get("chunk_count"), stage_states, flush=True)
                    last_print = now
                if all(stage_states.get(k) == "ready" for k in ("lexical", "graph")):
                    break
                if now > deadline:
                    raise TimeoutError("Index readiness")
                time.sleep(2)
            metadata["index_wall_seconds"] = time.monotonic() - started
            metadata["peak_sampled_rss_kib"] = max(rss_samples, default=0)
            metadata["index_bytes"] = sum(p.stat().st_size for p in (corpus / ".trusty-search").rglob("*") if p.is_file())
            metadata["status"] = status
            metadata["graph_stats"] = request(base, "/indexes/experiment/graph/stats")
            (treatment / "manifest.json").write_text(json.dumps(metadata, indent=2))
            verify_fixture(base, "experiment")
            queries = json.loads((ROOT / "queries.json").read_text())
            result = evaluate(base, "experiment", queries, args.split, 3)
            result["manifest"] = metadata
            (treatment / "results.json").write_text(json.dumps(result, indent=2))
            print(name, json.dumps(result["summary"]), flush=True)
        except Exception:
            failure: dict[str, Any] = {"traceback": traceback.format_exc()}
            try:
                failure["evidence"] = request(base, "/experiment/evidence")
            except Exception as error:
                failure["evidence_error"] = str(error)
            (treatment / "failure.json").write_text(json.dumps(failure, indent=2))
            raise
        finally:
            if process.poll() is None:
                process.send_signal(signal.SIGTERM)
                try:
                    process.wait(timeout=20)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=10)


if __name__ == "__main__":
    main()
