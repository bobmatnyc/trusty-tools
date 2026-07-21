"""Pure JSON-RPC 2.0 wire protocol handling for the embedding sidecar.

Why: the exact wire contract is defined by the Rust client
(`trusty-common/src/embedder_client/stdio.rs`) and the reference server
(`trusty-embedderd/src/stdio_server.rs`). Isolating the frame parse / build /
dispatch logic here — with the *encoder* injected — lets the protocol
conformance tests exercise id-echo, out-of-order tolerance, empty batch, and
error framing WITHOUT importing torch (a stub encoder is enough).

Wire contract (newline-framed, one JSON object per line):
  Request : {"jsonrpc":"2.0","method":"embed","params":{"texts":[...]},"id":<n>}
  Success : {"jsonrpc":"2.0","result":{"embeddings":[[..384 f32..],..],"device":"mps"},"id":<echo>}
  Error   : {"jsonrpc":"2.0","error":{"code":<i32>,"message":"..."},"id":<echo>}

  The optional ``device`` key on a success ``result`` (epic #3524 slice 5,
  issue #3493 P1) is a Python/MPS-sidecar-only extension: the ACTUAL torch
  device (``"mps"`` / ``"cuda"`` / ``"cpu"``) the encoder resolved, read from
  the encoder callable's ``device`` attribute (see ``model.build_encoder``).
  The reference Rust ``trusty-embedderd`` never emits this key; the Rust
  client (``StdioEmbedderClient``) treats it as optional (``#[serde(default)]``)
  so parsing an older/reference frame without it is unaffected.

Invariants honoured here:
  * echo the request ``id`` verbatim (the client correlates responses by id and
    discards any frame whose id it does not recognise — see stdio.rs).
  * empty ``texts`` -> ``embeddings: []`` (matches the Rust client short-circuit).
  * unknown method -> JSON-RPC -32601.
  * any exception in ``embed`` -> JSON-RPC -32603 error frame (never crash the
    loop over one bad request).
  * all-zero guard: the Rust HNSW upsert path rejects all-zero vectors, so a
    degenerate encode result is nudged to a valid unit vector rather than
    emitted as zeros.
"""

from __future__ import annotations

import json
from typing import Any, Callable, List, Optional

# JSON-RPC / model constants — kept in sync with the Rust side
# (`trusty-common::embedder::EMBED_DIM`).
JSONRPC_VERSION = "2.0"
METHOD_EMBED = "embed"
EMBED_DIM = 384

# JSON-RPC error codes (subset of the spec used by the reference server).
ERR_METHOD_NOT_FOUND = -32601
ERR_INTERNAL = -32603

# An encoder is any callable taking a list of texts and returning a list of
# 384-dim float lists (already L2-normalized). Injected so the protocol layer
# is torch-free and unit-testable.
Encoder = Callable[[List[str]], List[List[float]]]


def build_success(req_id: Any, embeddings: List[List[float]], device: Optional[str] = None) -> str:
    """Serialize a success response frame (terminating newline included).

    ``device`` (epic #3524 slice 5) is included as ``result.device`` when
    given — see the module docstring's wire-contract note. Omitted entirely
    when ``None`` so the frame shape matches the reference server exactly.
    """
    result: dict = {"embeddings": embeddings}
    if device:
        result["device"] = device
    return (
        json.dumps(
            {"jsonrpc": JSONRPC_VERSION, "result": result, "id": req_id},
            separators=(",", ":"),
        )
        + "\n"
    )


def build_error(req_id: Any, code: int, message: str) -> str:
    """Serialize an error response frame (terminating newline included)."""
    return (
        json.dumps(
            {"jsonrpc": JSONRPC_VERSION, "error": {"code": code, "message": message}, "id": req_id},
            separators=(",", ":"),
        )
        + "\n"
    )


def _sanitize(embeddings: List[List[float]]) -> List[List[float]]:
    """Apply the all-zero guard.

    Why: the Rust HNSW upsert rejects all-zero vectors. Real normalized text
    embeddings are never all-zero, but a pathological/degenerate encode (e.g.
    an empty string on some backends) could be — nudge the first component to
    1.0 so the vector stays a valid unit vector rather than emitting zeros.
    """
    out: List[List[float]] = []
    for v in embeddings:
        if not any(v):
            v = list(v)
            if v:
                v[0] = 1.0
            else:
                v = [1.0] + [0.0] * (EMBED_DIM - 1)
        out.append(v)
    return out


def handle_frame(line: str, encoder: Encoder) -> Optional[str]:
    """Dispatch one raw stdin line to a response frame.

    Returns the response frame string (newline-terminated) to write to stdout,
    or ``None`` for a blank line (nothing to emit). Never raises: any failure
    is converted to a JSON-RPC error frame echoing the request id when known.
    """
    stripped = line.strip()
    if not stripped:
        return None

    req_id: Any = None
    try:
        req = json.loads(stripped)
        req_id = req.get("id")
        method = req.get("method")

        if method != METHOD_EMBED:
            return build_error(req_id, ERR_METHOD_NOT_FOUND, f"method not found: {method!r}")

        params = req.get("params") or {}
        texts = params.get("texts") or []
        if len(texts) == 0:
            # Matches the Rust client's empty-batch short-circuit.
            return build_success(req_id, [])

        embeddings = _sanitize(encoder(texts))
        # Epic #3524 slice 5: echo the encoder's actually-resolved device, if
        # it set one (see `model.build_encoder`). `getattr` keeps this
        # protocol layer working unchanged against any torch-free stub
        # encoder (e.g. the conformance tests' `stub_encoder`) that never
        # sets the attribute.
        device = getattr(encoder, "device", None)
        return build_success(req_id, embeddings, device)
    except Exception as exc:  # noqa: BLE001 — report as JSON-RPC error, never crash the loop
        return build_error(req_id, ERR_INTERNAL, f"internal error: {exc}")
