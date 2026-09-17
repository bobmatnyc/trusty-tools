"""Tests ensure irrelevant same-file hits and overlapping chunks do not inflate recall."""
import pytest
from benchmark import quality, relevant, request, percentile, validate_complete


def test_relevance_requires_path_and_line_overlap():
    expected = {"path": "src/a.rs", "start_line": 90, "end_line": 90}
    assert not relevant({"path": "src/a.rs", "start_line": 1, "end_line": 20}, expected)
    assert not relevant({"path": "src/b.rs", "start_line": 80, "end_line": 100}, expected)
    assert relevant({"path": "src/a.rs", "start_line": 90, "end_line": 110}, expected)


def test_multi_target_recall_and_duplicates():
    expected = [{"path": "a", "start_line": n, "end_line": n} for n in (10, 100)]
    hits = [{"path": "a", "start_line": 1, "end_line": 20},
            {"path": "a", "start_line": 5, "end_line": 25}]
    metrics = quality(hits, expected)
    assert metrics["success_at_5"] == 1
    assert metrics["recall_at_10"] == .5
    assert metrics["duplicate_fraction"] == .5
    assert quality([], expected)["mrr_at_10"] == 0


@pytest.mark.parametrize("url", ["http://127.0.0.1:7878", "http://example.com:1234", "https://127.0.0.1:1234"])
def test_rejects_non_fixture_before_network(url):
    with pytest.raises(ValueError):
        request(url, "/health")


def test_percentile_interpolates():
    assert percentile([1, 2, 3], .5) == 2
    assert percentile([1, 2, 3], .95) == 2.9


@pytest.mark.parametrize("key,value", [("chunks_dropped_by_cap", 1), ("walk_truncated_by_budget", True),
    ("last_walk_error", "read failed"), ("promotion_deferred", "memory"),
    ("migration_error", "failed"), ("stuck_mid_walk", True), ("corpus_open_failure", "bad")])
def test_ready_stage_does_not_hide_incomplete_corpus(key, value):
    with pytest.raises(ValueError):
        validate_complete({"status": "ready", "chunk_count": 10, key: value})


def test_complete_corpus_and_no_vectors():
    validate_complete({"status": "ready", "chunk_count": 10})
    with pytest.raises(ValueError):
        validate_complete({"status": "ready", "chunk_count": 10, "semantic_coverage": {"vectors_present": 1}})
