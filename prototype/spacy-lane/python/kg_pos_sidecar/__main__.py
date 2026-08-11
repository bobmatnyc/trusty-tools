"""Newline-JSON-RPC-2.0 stdio server exposing spaCy POS tags + noun chunks.

Why: lane B of the #5399 bake-off needs spaCy resident so the ~0.5 s model load
is paid once per daemon lifetime, not once per `kg_extract` call. The wire shape
deliberately copies `trusty_embed_sidecar` (crates/trusty-embedderd-py) so the
prototype reuses this workspace's existing, shipped Python-sidecar pattern
rather than inventing a second one.
What: reads one JSON object per line on stdin, writes one per line on stdout;
stdout carries ONLY frames, every log goes to stderr. `analyze` returns, per
input text, the token stream (text, char offset, coarse POS, fine tag, OOV flag)
and the noun-chunk spans with their head-token index. All extraction POLICY
stays in Rust — this process reports linguistic facts and decides nothing.
Test: `prototype/spacy-lane/rust-harness` drives it end to end; `ping` is the
model-free latency floor.
"""

import json
import sys
import time


def _log(msg: str) -> None:
    print(msg, file=sys.stderr, flush=True)


def _load_nlp():
    """Load en_core_web_sm once, keeping only the pipes the KG gate consumes.

    Why: `ner` costs load time and per-call latency and contributes nothing to a
    POS/NP decision. The parser is NOT optional — spaCy's `doc.noun_chunks`
    is derived from the dependency parse and raises without it.
    """
    import spacy

    t0 = time.perf_counter()
    nlp = spacy.load("en_core_web_sm", exclude=["ner"])
    load_ms = (time.perf_counter() - t0) * 1000.0
    # One warmup parse before readiness is announced, so the first real request
    # does not pay lazy thinc/numpy initialisation.
    t1 = time.perf_counter()
    nlp("warmup sentence for the parser")
    warm_ms = (time.perf_counter() - t1) * 1000.0
    _log(f"spacy loaded pipes={nlp.pipe_names} load_ms={load_ms:.1f} warmup_ms={warm_ms:.1f}")
    return nlp, load_ms, warm_ms


def _analyze(nlp, texts):
    docs = []
    for doc in nlp.pipe(texts):
        tokens = [
            {
                "i": t.i,
                "text": t.text,
                "start": t.idx,
                "end": t.idx + len(t.text),
                "pos": t.pos_,
                "tag": t.tag_,
                "oov": bool(t.is_oov),
            }
            for t in doc
        ]
        chunks = [
            {
                "start": c.start_char,
                "end": c.end_char,
                "text": c.text,
                "root": c.root.i,
                "root_pos": c.root.pos_,
            }
            for c in doc.noun_chunks
        ]
        docs.append({"tokens": tokens, "noun_chunks": chunks})
    return docs


def serve() -> int:
    nlp, load_ms, warm_ms = _load_nlp()
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
        except json.JSONDecodeError as exc:
            print(
                json.dumps(
                    {
                        "jsonrpc": "2.0",
                        "error": {"code": -32700, "message": f"parse error: {exc}"},
                        "id": None,
                    }
                ),
                flush=True,
            )
            continue

        rid = req.get("id")
        method = req.get("method")
        params = req.get("params") or {}
        try:
            if method == "analyze":
                result = {"docs": _analyze(nlp, params.get("texts", []))}
            elif method == "ping":
                # Model-free floor: isolates wire+serde cost from spaCy cost.
                result = {"load_ms": load_ms, "warmup_ms": warm_ms}
            else:
                print(
                    json.dumps(
                        {
                            "jsonrpc": "2.0",
                            "error": {"code": -32601, "message": f"unknown method {method}"},
                            "id": rid,
                        }
                    ),
                    flush=True,
                )
                continue
            print(json.dumps({"jsonrpc": "2.0", "result": result, "id": rid}), flush=True)
        except Exception as exc:  # one bad request must never kill the loop
            print(
                json.dumps(
                    {
                        "jsonrpc": "2.0",
                        "error": {"code": -32603, "message": f"{type(exc).__name__}: {exc}"},
                        "id": rid,
                    }
                ),
                flush=True,
            )
    return 0


if __name__ == "__main__":
    sys.exit(serve())
