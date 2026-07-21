"""Model loading, device selection, and the real SentenceTransformer encoder.

Why: keeps every torch/sentence-transformers import behind function calls so
``protocol.py`` and ``sidecar.py`` (and their conformance tests) stay
torch-free. Only ``build_encoder`` actually imports torch.

Device policy (``TRUSTY_DEVICE`` = ``cpu`` | ``gpu`` | ``auto``):
  * ``auto`` (default): MPS if available (Apple Silicon), else CUDA, else CPU.
  * ``gpu``: MPS if available, else CUDA, else CPU (with a loud warning — the
    sidecar must not hard-fail; the Rust launcher falls back to the ort path
    only on a *non-zero exit*, so a working CPU sidecar is preferable to a
    crash).
  * ``cpu``: force CPU.

dtype: fp32 by default (numerically identical to the CPU reference per the
spike); ``TRUSTY_PY_EMBED_FP16=1`` opts into fp16 on MPS/CUDA (~1.3x faster,
cosine still >= 0.9999 per the spike).
"""

from __future__ import annotations

import os
from typing import Callable, List

# Disable HuggingFace tokenizers' internal (Rust-side) parallelism before
# `sentence_transformers`/`transformers` are ever imported — the library reads
# this env var once, at import time, so it must be set here at module load
# (this module is imported eagerly by `__main__`, well before `build_encoder`
# performs its lazy `import torch` / `from sentence_transformers import ...`
# below). Why: without it, tokenizers spawns a `multiprocessing.resource_tracker`
# helper process to back parallel tokenization the sidecar never benefits from
# (one request encoded at a time by the single worker thread — see
# `sidecar.py`), leaving an extra orphan-prone child in the process tree plus
# spurious "leaked semaphore" warnings on shutdown. `setdefault` so an
# operator's explicit env override is never clobbered.
os.environ.setdefault("TOKENIZERS_PARALLELISM", "false")

MODEL_NAME = "sentence-transformers/all-MiniLM-L6-v2"
# Pin the model to a specific HuggingFace revision so a bootstrapped venv always
# downloads bit-identical weights (reproducibility — the analogue of the pinned
# uv.lock for the Python deps). This is the long-stable main-branch commit of
# all-MiniLM-L6-v2.
MODEL_REVISION = "c9745ed1d9f207416be6d2e6f8de32d1f16199bf"
EMBED_DIM = 384

# Default encode batch size when the supervisor forwards none. MPS unified
# memory can back large batches; the clamp below still bounds it.
DEFAULT_BATCH_SIZE = 256
# Upper bound on the per-encode batch on MPS. Apple Silicon shares one unified
# memory pool between CPU and GPU, so an unbounded batch (the parent may forward
# a large TRUSTY_EMBED_BATCH_SIZE) can spike RSS and trigger jetsam. Clamp the
# effective batch on MPS to keep the sidecar well under the ~800 MB peak the
# spike measured.
MPS_BATCH_CLAMP = 512


def resolve_device(requested: str, mps_available: bool, cuda_available: bool) -> str:
    """Pure device resolution — unit-testable without torch.

    ``requested`` is the (lowercased) ``TRUSTY_DEVICE`` value. Returns one of
    ``"mps"``, ``"cuda"``, or ``"cpu"``.
    """
    requested = (requested or "auto").strip().lower()
    if requested == "cpu":
        return "cpu"
    if requested in ("gpu", "auto"):
        if mps_available:
            return "mps"
        if cuda_available:
            return "cuda"
        return "cpu"
    # Unknown value — be conservative and auto-select.
    if mps_available:
        return "mps"
    if cuda_available:
        return "cuda"
    return "cpu"


def resolve_batch_size(device: str, forwarded: int, py_override: int) -> int:
    """Pure batch-size resolution — unit-testable without torch.

    ``py_override`` (0 = unset) wins over ``forwarded`` (the supervisor's
    ``TRUSTY_EMBED_BATCH_SIZE``); the result is clamped to ``MPS_BATCH_CLAMP``
    on MPS and to at least 1 everywhere.
    """
    batch = py_override if py_override > 0 else forwarded
    if batch <= 0:
        batch = DEFAULT_BATCH_SIZE
    if device == "mps":
        batch = min(batch, MPS_BATCH_CLAMP)
    return max(batch, 1)


def _env_int(name: str, default: int) -> int:
    try:
        return int(os.environ.get(name, "").strip() or default)
    except ValueError:
        return default


def build_encoder(log=lambda _m: None) -> Callable[[List[str]], List[List[float]]]:
    """Load the model, select the device, warm up, and return an ``encode`` fn.

    The returned callable maps ``List[str] -> List[List[float]]`` (384-dim,
    L2-normalized). The model + one warmup embed run BEFORE this returns so the
    supervisor's real-embed readiness probe never races a cold torch import or
    first-MPS-compile.
    """
    import time

    t0 = time.perf_counter()
    import numpy as np
    import torch
    from sentence_transformers import SentenceTransformer

    t_import = time.perf_counter() - t0

    requested = os.environ.get("TRUSTY_DEVICE", "auto")
    mps_available = bool(getattr(torch.backends, "mps", None)) and torch.backends.mps.is_available()
    cuda_available = torch.cuda.is_available()
    device = resolve_device(requested, mps_available, cuda_available)
    if requested.strip().lower() == "gpu" and device == "cpu":
        log("WARNING: TRUSTY_DEVICE=gpu but no MPS/CUDA device available — using CPU")

    fp16 = os.environ.get("TRUSTY_PY_EMBED_FP16", "").strip() in ("1", "true", "yes", "on")
    torch_dtype = torch.float16 if (fp16 and device in ("mps", "cuda")) else torch.float32

    forwarded = _env_int("TRUSTY_EMBED_BATCH_SIZE", DEFAULT_BATCH_SIZE)
    py_override = _env_int("TRUSTY_PY_EMBED_BATCH_SIZE", 0)
    batch_size = resolve_batch_size(device, forwarded, py_override)

    t1 = time.perf_counter()
    model = SentenceTransformer(
        MODEL_NAME,
        device=device,
        revision=MODEL_REVISION,
        model_kwargs={"torch_dtype": torch_dtype},
    )
    t_load = time.perf_counter() - t1

    def encode(texts: List[str]) -> List[List[float]]:
        vecs = model.encode(
            texts,
            batch_size=batch_size,
            normalize_embeddings=True,
            show_progress_bar=False,
            convert_to_numpy=True,
        )
        return [v.astype(np.float32).tolist() for v in vecs]

    # One real warmup embed to trigger MPS kernel compilation before the first
    # request is ever read.
    t2 = time.perf_counter()
    _ = encode(["trusty-embed-sidecar startup warmup"])
    t_warm = time.perf_counter() - t2

    log(
        f"ready: device={device} dtype={'fp16' if torch_dtype is torch.float16 else 'fp32'} "
        f"batch_size={batch_size} dim={model.get_sentence_embedding_dimension()} "
        f"torch_import={t_import:.2f}s model_load={t_load:.2f}s warmup={t_warm:.2f}s "
        f"cold_total={t_import + t_load + t_warm:.2f}s"
    )
    # Epic #3524 slice 5 / issue #3493 P1: attach the ACTUALLY-resolved device
    # to the returned callable so `protocol.py` can echo it in every response
    # frame. The Rust `/health` handler otherwise only has a build-features
    # PREDICTION (`resolve_expected_provider()`) to go on, which is wrong for
    # this sidecar — it guesses the ORT-flavoured `CoreML(ANE)` label while
    # torch actually selected `mps`. A plain function attribute (rather than
    # threading a `(encode, device)` tuple through every caller) keeps
    # `Encoder` a simple `Callable[[List[str]], List[List[float]]]`.
    encode.device = device  # type: ignore[attr-defined]
    return encode
