"""trusty_embed_sidecar — Python/MPS embedding sidecar for trusty-search.

Speaks the newline-framed JSON-RPC 2.0 ``embed`` wire protocol expected by
``trusty-common::embedder_client::StdioEmbedderClient`` (epic #3524, slices
2-4). Run as ``python -m trusty_embed_sidecar --stdio``.

The protocol layer (``protocol``) and serve loop (``sidecar``) are torch-free
and unit-tested via a stub encoder; ``model`` holds the real
SentenceTransformer/torch encoder.
"""

from .protocol import EMBED_DIM, build_error, build_success, handle_frame
from .sidecar import serve

__version__ = "0.1.0"

__all__ = [
    "EMBED_DIM",
    "build_error",
    "build_success",
    "handle_frame",
    "serve",
    "__version__",
]
