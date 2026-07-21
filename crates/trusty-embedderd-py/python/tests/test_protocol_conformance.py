"""Protocol conformance tests for the embedding sidecar.

These run WITHOUT torch: a deterministic stub encoder stands in for the real
SentenceTransformer, so CI can prove the wire contract (id-echo, out-of-order
tolerance, empty batch, error framing, EOF/clean-shutdown) with no model,
no MPS, and no network.

The contract mirrors `trusty-common/src/embedder_client/stdio.rs` and the
reference `trusty-embedderd/src/stdio_server.rs`.
"""

from __future__ import annotations

import io
import json
import math
from typing import List

from trusty_embed_sidecar.protocol import (
    EMBED_DIM,
    ERR_INTERNAL,
    ERR_METHOD_NOT_FOUND,
    handle_frame,
)
from trusty_embed_sidecar.sidecar import serve


def stub_encoder(texts: List[str]) -> List[List[float]]:
    """Deterministic, torch-free encoder: a unit vector whose first component
    encodes ``len(text)`` so tests can assert per-text distinctness. Always
    384-dim and L2-normalized."""
    out: List[List[float]] = []
    for t in texts:
        v = [0.0] * EMBED_DIM
        # Two non-zero components -> genuinely normalizable, never all-zero.
        v[0] = float(len(t) + 1)
        v[1] = 1.0
        norm = math.sqrt(sum(x * x for x in v))
        out.append([x / norm for x in v])
    return out


def _req(rid, texts, method="embed"):
    return json.dumps({"jsonrpc": "2.0", "method": method, "params": {"texts": texts}, "id": rid})


def test_id_echo_verbatim():
    for rid in (1, 7, 999, 2**40):
        resp = json.loads(handle_frame(_req(rid, ["hello"]), stub_encoder))
        assert resp["id"] == rid
        assert resp["jsonrpc"] == "2.0"
        assert "result" in resp


def test_result_shape_dim_and_unit_norm():
    resp = json.loads(handle_frame(_req(42, ["alpha", "beta gamma"]), stub_encoder))
    embs = resp["result"]["embeddings"]
    assert len(embs) == 2
    for v in embs:
        assert len(v) == EMBED_DIM
        assert abs(math.sqrt(sum(x * x for x in v)) - 1.0) < 1e-5
        assert any(v), "vector must never be all-zero"


def test_empty_batch_returns_empty_list():
    resp = json.loads(handle_frame(_req(5, []), stub_encoder))
    assert resp["result"]["embeddings"] == []
    assert resp["id"] == 5


def test_out_of_order_tolerance_ids_are_independent():
    # The client correlates by id; the sidecar must echo whatever id it is
    # given, in whatever order, so a client can dispatch responses arriving in
    # any order. Interleave several ids and assert each response carries its own.
    ids = [10, 3, 77, 4, 55]
    for rid in ids:
        resp = json.loads(handle_frame(_req(rid, [f"text-{rid}"]), stub_encoder))
        assert resp["id"] == rid


def test_unknown_method_is_method_not_found():
    resp = json.loads(handle_frame(_req(9, ["x"], method="bogus"), stub_encoder))
    assert resp["error"]["code"] == ERR_METHOD_NOT_FOUND
    assert resp["id"] == 9
    assert "result" not in resp


def test_malformed_json_yields_internal_error_frame():
    resp = json.loads(handle_frame("{not valid json", stub_encoder))
    assert resp["error"]["code"] == ERR_INTERNAL
    assert resp["id"] is None


def test_encoder_exception_becomes_error_frame_not_crash():
    def boom(_texts):
        raise RuntimeError("encoder exploded")

    resp = json.loads(handle_frame(_req(12, ["x"]), boom))
    assert resp["error"]["code"] == ERR_INTERNAL
    assert resp["id"] == 12
    assert "encoder exploded" in resp["error"]["message"]


def test_blank_line_produces_no_frame():
    assert handle_frame("   \n", stub_encoder) is None
    assert handle_frame("", stub_encoder) is None


def test_serve_loop_end_to_end_over_pipes():
    # Drive the real serve loop (reader + worker threads) with in-memory pipes,
    # exactly as the supervisor drives it over OS pipes. EOF on the input stream
    # must drain queued work and return cleanly.
    reqs = [
        _req(1, ["one"]),
        _req(2, []),
        _req(3, ["two", "three"]),
        _req(4, ["x"], method="nope"),
    ]
    stdin = io.StringIO("\n".join(reqs) + "\n")
    stdout = io.StringIO()

    serve(stub_encoder, stdin=stdin, stdout=stdout)

    lines = [ln for ln in stdout.getvalue().splitlines() if ln.strip()]
    responses = {json.loads(ln)["id"]: json.loads(ln) for ln in lines}
    assert set(responses) == {1, 2, 3, 4}
    assert len(responses[1]["result"]["embeddings"]) == 1
    assert responses[2]["result"]["embeddings"] == []
    assert len(responses[3]["result"]["embeddings"]) == 2
    assert responses[4]["error"]["code"] == ERR_METHOD_NOT_FOUND


def test_device_and_batch_resolution_pure_helpers():
    # Pure helpers in model.py are import-safe without torch.
    from trusty_embed_sidecar.model import MPS_BATCH_CLAMP, resolve_batch_size, resolve_device

    assert resolve_device("cpu", mps_available=True, cuda_available=True) == "cpu"
    assert resolve_device("auto", mps_available=True, cuda_available=False) == "mps"
    assert resolve_device("auto", mps_available=False, cuda_available=True) == "cuda"
    assert resolve_device("auto", mps_available=False, cuda_available=False) == "cpu"
    assert resolve_device("gpu", mps_available=False, cuda_available=False) == "cpu"

    # py override wins over forwarded; MPS clamps; floor at 1.
    assert resolve_batch_size("mps", forwarded=256, py_override=1024) == MPS_BATCH_CLAMP
    assert resolve_batch_size("cpu", forwarded=256, py_override=1024) == 1024
    assert resolve_batch_size("cpu", forwarded=64, py_override=0) == 64
    assert resolve_batch_size("cpu", forwarded=0, py_override=0) == 256
    assert resolve_batch_size("mps", forwarded=-5, py_override=0) == 256
