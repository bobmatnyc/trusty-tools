"""Offline presentation ablation over saved tuning-v3 HTTP results only.

The TOC retains every source locator and original rank, but intentionally omits
most source, scores, and match explanations. It is a navigation aid, not an
equivalent replacement for evidence. The saved raw HTTP results remain primary.
Run: venv/bin/python presentation.py runs/c0w100-tuning-v3/results.json
"""
from __future__ import annotations

import argparse
import json
import statistics
from pathlib import Path, PurePosixPath
from typing import Any, TypedDict

from benchmark import compact, size


HitRow = tuple[int, int, int, str | None, str]


class SourceFile(TypedDict):
    path: str
    hits: list[HitRow]


class NavigationTOC(TypedDict):
    index: str
    columns: tuple[str, ...]
    files: list[SourceFile]


def navigation_toc(index: str, hits: list[dict[str, Any]]) -> NavigationTOC:
    """Group paths once; keep explicit original ranks and exact source ranges.

    Preconditions: each hit has a relative path, valid inclusive line range,
    source content, and optional symbol. The cue is at most twelve source words.
    No files are opened and no inferred internal line positions are introduced.
    """
    if not index:
        raise ValueError("Index identity is required")
    grouped: dict[str, SourceFile] = {}
    for rank, hit in enumerate(hits, 1):
        path = hit.get("path")
        start, end = hit.get("start_line"), hit.get("end_line")
        content, symbol = hit.get("content"), hit.get("function_name")
        if not isinstance(path, str) or not path or PurePosixPath(path).is_absolute():
            raise ValueError("A nonempty relative source path is required")
        if ".." in PurePosixPath(path).parts:
            raise ValueError("Source path must stay inside the indexed root")
        if type(start) is not int or type(end) is not int or start < 1 or end < start:
            raise ValueError("Source ranges must be positive and inclusive")
        if not isinstance(content, str) or not (symbol is None or isinstance(symbol, str)):
            raise ValueError("Source content and optional symbol must be strings")
        cue = " ".join(content.split()[:12])
        group = grouped.setdefault(path, {"path": path, "hits": []})
        group["hits"].append((rank, start, end, symbol, cue))
    return {"index": index, "columns": ("rank", "start", "end", "symbol", "cue"),
            "files": list(grouped.values())}


def compare_tuning(path: Path) -> dict[str, Any]:
    """Measure identical saved hits; refuse held-out and earlier run artifacts."""
    if path.name != "results.json" or not path.parent.name.endswith("-tuning-v3"):
        raise ValueError("Only explicit tuning-v3/results.json artifacts are accepted")
    document = json.loads(path.read_text())
    if document.get("split") != "tuning":
        raise ValueError("Presentation tuning cannot read held-out results")
    rows: list[dict[str, Any]] = []
    for saved in document["rows"]:
        hits = saved["response"]["results"]
        toc = navigation_toc("experiment", hits)
        rows.append({"id": saved["id"], "lane": saved["lane"], "hits": len(hits),
                     "sizes": {"compact": size(compact(hits)),
                               "cards": size(saved["cards"]), "toc": size(toc)}})
    summary: dict[str, Any] = {}
    for lane in ("all", "lexical", "graph"):
        selected = [row for row in rows if lane == "all" or row["lane"] == lane]
        metrics: dict[str, Any] = {"result_sets": len(selected),
                                   "hits": sum(row["hits"] for row in selected)}
        for presentation in ("compact", "cards", "toc"):
            for unit in ("bytes", "tokens"):
                values = [row["sizes"][presentation][unit] for row in selected]
                metrics[f"mean_{presentation}_{unit}"] = statistics.mean(values) if values else 0
        for baseline in ("compact", "cards"):
            denominator = metrics[f"mean_{baseline}_tokens"]
            metrics[f"toc_token_reduction_vs_{baseline}"] = (
                1 - metrics["mean_toc_tokens"] / denominator if denominator else 0)
        summary[lane] = metrics
    return {"raw_results": str(path.resolve()), "treatment": path.parent.name,
            "split": "tuning", "tokenizer": "cl100k_base",
            "measurement": "offline canonical JSON serialization; not HTTP wire bytes",
            "limitation": "TOC omits full evidence, scores, and match explanations; raw results retain them",
            "summary": summary, "rows": rows}


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("results", nargs="+", type=Path)
    arguments = parser.parse_args()
    print(json.dumps([compare_tuning(path) for path in arguments.results], indent=2))


if __name__ == "__main__":
    main()
