"""#9828: the encoder frees the MPS cache after each encode, and only on MPS.

Torch-free: a stub model, a stub numpy and a stub ``torch`` namespace stand in
for the real libraries, so the test runs with no model, no MPS and no network.
"""

from __future__ import annotations

from types import SimpleNamespace

from trusty_embed_sidecar.model import make_encode


class _Vec:
    def __init__(self, values):
        self._values = values

    def astype(self, _dtype):
        return self

    def tolist(self):
        return list(self._values)


class _Model:
    def __init__(self, events):
        self.events = events

    def encode(self, texts, **_kwargs):
        self.events.append("encode")
        return [_Vec([0.5, 0.5]) for _ in texts]


_NP = SimpleNamespace(float32="float32")


def _torch_with_mps(events):
    return SimpleNamespace(mps=SimpleNamespace(empty_cache=lambda: events.append("empty_cache")))


def test_mps_empties_the_cache_after_each_encode():
    events = []
    encode = make_encode(_Model(events), 8, "mps", _torch_with_mps(events), _NP)
    assert encode(["a", "b"]) == [[0.5, 0.5], [0.5, 0.5]]
    encode(["c"])
    assert events == ["encode", "empty_cache", "encode", "empty_cache"]


def test_cpu_and_cuda_never_touch_the_mps_cache():
    for device in ("cpu", "cuda"):
        events = []
        encode = make_encode(_Model(events), 8, device, _torch_with_mps(events), _NP)
        encode(["a"])
        assert events == ["encode"], device


def test_missing_or_failing_torch_mps_does_not_break_encode():
    events = []
    no_mps = SimpleNamespace()
    assert make_encode(_Model(events), 8, "mps", no_mps, _NP)(["a"]) == [[0.5, 0.5]]

    def boom():
        raise RuntimeError("MPS backend unavailable")

    logged = []
    failing = SimpleNamespace(mps=SimpleNamespace(empty_cache=boom))
    encode = make_encode(_Model(events), 8, "mps", failing, _NP, log=logged.append)
    assert encode(["a"]) == [[0.5, 0.5]]
    encode(["b"])
    assert len(logged) == 1, "a failing empty_cache is logged once, not per encode"
