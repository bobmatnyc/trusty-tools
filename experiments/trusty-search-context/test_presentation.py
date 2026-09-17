from __future__ import annotations

import copy
import json
from pathlib import Path
from typing import Any

import pytest

from presentation import compare_tuning, navigation_toc


def hit(path: str, start: int, end: int, content: str = "fn café() {}") -> dict[str, Any]:
    return {"path": path, "start_line": start, "end_line": end,
            "function_name": "café", "content": content, "score": 0.4,
            "compact_snippet": content, "match_reason": "bm25"}


def test_preserves_every_locator_and_rank_with_interleaved_files() -> None:
    hits = [hit("src/a.rs", 10, 18), hit("src/b.rs", 21, 27),
            hit("src/a.rs", 12, 16), hit("src/a.rs", 10, 18)]
    original = copy.deepcopy(hits)
    toc = navigation_toc("experiment", hits)
    assert [group["path"] for group in toc["files"]] == ["src/a.rs", "src/b.rs"]
    recovered = sorted((row[0], toc["index"], group["path"], row[1], row[2], row[3])
                       for group in toc["files"] for row in group["hits"])
    assert recovered == [(rank, "experiment", h["path"], h["start_line"], h["end_line"], h["function_name"])
                         for rank, h in enumerate(hits, 1)]
    assert hits == original
    assert navigation_toc("experiment", hits) == toc


def test_cues_are_bounded_source_words_without_inferred_line_offsets() -> None:
    source = "\n\n// 雪 and café\n" + " ".join(f"word{n}" for n in range(30))
    toc = navigation_toc("experiment", [hit("src/雪.rs", 89, 94, source)])
    row = toc["files"][0]["hits"][0]
    assert row[1:3] == (89, 94)
    assert row[4].split() == source.split()[:12]
    assert len(row[4].split()) <= 12
    assert toc["columns"] == ("rank", "start", "end", "symbol", "cue")
    assert len(row) == 5  # No invented cue/internal declaration line number.


def test_empty_results_content_and_unknown_symbol() -> None:
    assert navigation_toc("experiment", [])["files"] == []
    empty = hit("empty.md", 1, 1, "")
    empty["function_name"] = None
    assert navigation_toc("experiment", [empty])["files"][0]["hits"] == [(1, 1, 1, None, "")]


@pytest.mark.parametrize("field,value", [("path", "/absolute.rs"), ("path", "../escape.rs"),
                                          ("start_line", 0), ("end_line", 1),
                                          ("start_line", True), ("content", None)])
def test_invalid_locators_are_rejected(field: str, value: Any) -> None:
    invalid = hit("src/a.rs", 2, 4)
    invalid[field] = value
    with pytest.raises(ValueError):
        navigation_toc("experiment", [invalid])


def test_comparison_uses_saved_tuning_and_refuses_held_out(tmp_path: Path) -> None:
    saved = tmp_path / "c0w100-tuning-v3" / "results.json"
    saved.parent.mkdir()
    source_hits = [hit("src/a.rs", 2, 9)]
    document = {"split": "tuning", "rows": [{"id": "T1", "lane": "lexical",
                "response": {"results": source_hits}, "cards": [{"description": "source"}]}]}
    saved.write_text(json.dumps(document))
    report = compare_tuning(saved)
    assert report["summary"]["all"]["result_sets"] == 1
    assert report["summary"]["all"]["hits"] == 1
    assert report["summary"]["graph"]["mean_toc_tokens"] == 0
    assert report["summary"]["all"]["mean_toc_tokens"] > 0
    with pytest.raises(ValueError, match="Only explicit"):
        compare_tuning(tmp_path / "held-out" / "results.json")
    document["split"] = "held_out"
    saved.write_text(json.dumps(document))
    with pytest.raises(ValueError, match="cannot read held-out"):
        compare_tuning(saved)
