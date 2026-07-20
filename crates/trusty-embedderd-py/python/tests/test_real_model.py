"""Real-model correctness gate (skipped when torch/model are unavailable).

Marked ``@pytest.mark.real_model`` and auto-skipped unless torch +
sentence-transformers import AND ``TRUSTY_RUN_REAL_MODEL=1`` is set (the model
download/load is slow and needs network on a cold cache). When it runs, it
embeds the exact 5 ``REFERENCE_TEXTS`` from
``trusty-common/src/embedder/mod.rs`` and asserts mean cosine >= 0.999 vs the
committed CPU-fp32 sentence-transformers reference vectors — the same >= 0.999
gate the Rust side enforces.
"""

from __future__ import annotations

import json
import math
import os
from pathlib import Path

import pytest

REF = Path(__file__).parent / "reference_vectors.json"


def _torch_available() -> bool:
    try:
        import sentence_transformers  # noqa: F401
        import torch  # noqa: F401

        return True
    except Exception:
        return False


pytestmark = pytest.mark.real_model


def _cosine(a, b):
    dot = sum(x * y for x, y in zip(a, b))
    na = math.sqrt(sum(x * x for x in a))
    nb = math.sqrt(sum(y * y for y in b))
    return dot / (na * nb)


@pytest.mark.skipif(
    not _torch_available() or os.environ.get("TRUSTY_RUN_REAL_MODEL") != "1",
    reason="real model requires torch + sentence-transformers and TRUSTY_RUN_REAL_MODEL=1",
)
def test_reference_cosine_gate_ge_0999():
    from trusty_embed_sidecar.model import build_encoder

    ref = json.loads(REF.read_text())
    texts, ref_vecs = ref["texts"], ref["vectors"]

    encode = build_encoder(log=lambda m: print(m))
    got = encode(texts)

    assert len(got) == len(ref_vecs)
    sims = [_cosine(g, r) for g, r in zip(got, ref_vecs)]
    mean_sim = sum(sims) / len(sims)
    min_sim = min(sims)
    print(f"reference gate: mean_cosine={mean_sim:.6f} min_cosine={min_sim:.6f}")

    # Invariants the Rust HNSW path relies on.
    for g in got:
        assert len(g) == 384
        assert abs(math.sqrt(sum(x * x for x in g)) - 1.0) < 1e-3
        assert any(g)

    assert mean_sim >= 0.999, f"mean cosine {mean_sim:.6f} below 0.999 gate"
