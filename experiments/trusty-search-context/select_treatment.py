"""Apply the precommitted tuning-only decision rule and preserve its inputs."""
from __future__ import annotations

import json
import statistics
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parent


def choose(runs: list[dict[str, Any]]) -> dict[str, Any]:
    if len(runs) != 6 or any(run["split"] != "tuning" for run in runs):
        raise ValueError("Require six tuning cells; held-out scores must not select a treatment")
    for field in ("binary_sha256", "archive_sha256", "query_sha256"):
        if len({run["manifest"][field] for run in runs}) != 1:
            raise ValueError(f"Inconsistent {field}")
    if len({run["status"]["last_walk_files_seen"] for run in runs}) != 1:
        raise ValueError("Corpus coverage changed across treatments")
    baseline = next(run for run in runs if run["manifest"]["words"] == 0 and run["manifest"]["window"] == 100)
    def score(run: dict[str, Any], metric: str) -> float:
        return statistics.mean(float(run["summary"][lane]["all"][metric]) for lane in ("lexical", "graph"))
    eligible = []
    for run in runs:
        exact_loss = [sum(r["quality"]["success_at_5"] for r in baseline["rows"] if r["lane"] == lane and r["type"] == "exact_symbol")
                      - sum(r["quality"]["success_at_5"] for r in run["rows"] if r["lane"] == lane and r["type"] == "exact_symbol")
                      for lane in ("lexical", "graph")]
        if score(run, "success_at_5") >= score(baseline, "success_at_5") and max(exact_loss) <= 1:
            eligible.append(run)
    winner = max(eligible, key=lambda run: (score(run, "success_at_5"), score(run, "mrr_at_10"),
                 -score(run, "mean_compact_tokens"), -run["manifest"]["index_bytes"]))
    return {"selected": winner["manifest"]["name"], "words": winner["manifest"]["words"],
            "window": winner["manifest"]["window"], "eligible": [r["manifest"]["name"] for r in eligible],
            "selection_metric": "mean lexical/graph success@5, then MRR@10, then compact tokens, then index bytes",
            "treatments": [{"name": r["manifest"]["name"], "success5": score(r, "success_at_5"),
                            "mrr10": score(r, "mrr_at_10"), "compact_tokens": score(r, "mean_compact_tokens"),
                            "card_tokens": score(r, "mean_cards_tokens"), "full_tokens": score(r, "mean_full_tokens"),
                            "index_seconds": r["manifest"]["index_wall_seconds"], "chunks": r["status"]["chunk_count"]} for r in runs]}


def main() -> None:
    runs = [json.loads(path.read_text()) for path in sorted((ROOT / "runs").glob("*-tuning-v3/results.json"))]
    result = choose(runs)
    (ROOT / "results/selection.json").write_text(json.dumps(result, indent=2))
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
