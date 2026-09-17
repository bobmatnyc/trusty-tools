"""Construct pinned cl100k_base from local cache bytes without a download fallback."""
from __future__ import annotations
import base64
import hashlib
import os
from pathlib import Path
import tempfile
import tiktoken

CACHE_KEY = '9b5ad71b2ce5302211f9c61530b329a4922fc6a4'
CONTENT_SHA256 = '223921b76ee99bde995b7ff738513eef100fb51d18c93597a113bcffe865b2a7'
# cl100k_base configuration from the pinned tiktoken 0.14.0 openai_public constructor.
PATTERN = r"'(?i:[sdmt]|ll|ve|re)|[^\r\n\p{L}\p{N}]?+\p{L}++|\p{N}{1,3}+| ?[^\s\p{L}\p{N}]++[\r\n]*+|\s++$|\s*[\r\n]|\s+(?!\S)|\s"


def load_encoding(cache_dir: Path | None = None) -> tiktoken.Encoding:
    """Fail closed if the pinned local tokenizer cache is absent or corrupt."""
    if cache_dir is None:
        location = os.environ.get('TIKTOKEN_CACHE_DIR',os.environ.get('DATA_GYM_CACHE_DIR',
            str(Path(tempfile.gettempdir())/'data-gym-cache')))
        if not location:
            raise ValueError('A local tokenizer cache is required; see README setup')
        cache_dir = Path(location)
    payload = (cache_dir/CACHE_KEY).read_bytes()
    if hashlib.sha256(payload).hexdigest() != CONTENT_SHA256:
        raise ValueError('Local tokenizer cache checksum mismatch')
    ranks = {base64.b64decode(token,validate=True):int(rank)
             for token,rank in (line.split() for line in payload.splitlines())}
    return tiktoken.Encoding(name='cl100k_base',pat_str=PATTERN,mergeable_ranks=ranks,
        special_tokens={'<|endoftext|>':100257,'<|fim_prefix|>':100258,
                        '<|fim_middle|>':100259,'<|fim_suffix|>':100260,'<|endofprompt|>':100276})
