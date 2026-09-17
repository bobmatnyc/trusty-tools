"""Explicit resident Rust process and pinned local CPU embedding adapters."""
from __future__ import annotations
import hashlib
import json
from pathlib import Path
import subprocess
from time import perf_counter_ns
from typing import cast
import numpy as np
from numpy.typing import NDArray
import onnxruntime as ort  # type: ignore[import-untyped]
from tokenizers import Tokenizer
from records import JSON, ModelArtifactError, ProtocolError, obj, integer

MODEL_HASH = 'bbd7b466f6d58e646fdc2bd5fd67b2f5e93c0b687011bd4548c420f7bd46f0c5'
TOKENIZER_HASH = 'da0e79933b9ed51798a3ae27893d3c5fa4a201126cef75586296df9b4d2c62a0'

class RustHelper:
    """One JSONL child; caller owns lifecycle, scratch storage, and all input."""
    def __init__(self, path: Path) -> None:
        started = perf_counter_ns()
        self.process = subprocess.Popen([str(path.resolve())], stdin=subprocess.PIPE,
            stdout=subprocess.PIPE, stderr=None, text=True, bufsize=1)
        self.counter = 0
        self.request({'op': 'format', 'triples': []})
        self.startup_ns = perf_counter_ns() - started

    def request(self, operation: dict[str, JSON]) -> dict[str, JSON]:
        self.counter += 1
        request_id = str(self.counter)
        if self.process.stdin is None or self.process.stdout is None:
            raise ProtocolError('helper pipes unavailable')
        self.process.stdin.write(json.dumps({'id': request_id, **operation}) + '\n')
        self.process.stdin.flush()
        line = self.process.stdout.readline()
        if not line:
            raise ProtocolError(f'helper closed unexpectedly: {self.process.poll()}')
        response = obj(json.loads(line))
        if response.get('id') != request_id or response.get('ok') is not True:
            raise ProtocolError(str(response))
        result = obj(response['result'])
        result['_helper_ns'] = integer(response['elapsed_ns'])
        return result

    def close(self) -> None:
        if self.process.poll() is None:
            try:
                self.request({'op': 'close'})
                self.process.wait(timeout=10)
            finally:
                if self.process.poll() is None:
                    self.process.kill()
                    self.process.wait()
        if self.process.stdin:
            self.process.stdin.close()
        if self.process.stdout:
            self.process.stdout.close()

class LocalEncoder:
    """Pinned fp32 MiniLM, one CPU thread, mean pooling, normalized vectors."""
    def __init__(self, model_dir: Path) -> None:
        started = perf_counter_ns()
        for filename, expected in [('model.onnx', MODEL_HASH), ('tokenizer.json', TOKENIZER_HASH)]:
            path = model_dir / filename
            if not path.is_file() or hashlib.sha256(path.read_bytes()).hexdigest() != expected:
                raise ModelArtifactError(f'missing or mismatched local artifact: {filename}')
        options = ort.SessionOptions()
        options.intra_op_num_threads = 1
        options.inter_op_num_threads = 1
        self.session = ort.InferenceSession(str(model_dir / 'model.onnx'), sess_options=options,
            providers=['CPUExecutionProvider'])
        self.tokenizer = Tokenizer.from_file(str(model_dir / 'tokenizer.json'))
        self.tokenizer.enable_truncation(max_length=256)
        self.tokenizer.enable_padding(pad_id=0, pad_token='[PAD]')
        self.full_tokenizer = Tokenizer.from_file(str(model_dir / 'tokenizer.json'))
        self.full_tokenizer.no_truncation()
        self.full_tokenizer.no_padding()
        self.load_ns = perf_counter_ns() - started
        self.truncation_events = 0
        self.dimension = 384

    def encode(self, texts: tuple[str, ...], *, corpus: bool = False) -> NDArray[np.float32]:
        if not texts:
            return np.empty((0, self.dimension), dtype=np.float32)
        if corpus:
            self.truncation_events += sum(len(self.full_tokenizer.encode(t).ids) > 256 for t in texts)
        batches: list[NDArray[np.float32]] = []
        for offset in range(0, len(texts), 32):
            tokens = self.tokenizer.encode_batch(list(texts[offset:offset + 32]))
            inputs = {'input_ids': np.asarray([t.ids for t in tokens], dtype=np.int64),
                'attention_mask': np.asarray([t.attention_mask for t in tokens], dtype=np.int64),
                'token_type_ids': np.asarray([t.type_ids for t in tokens], dtype=np.int64)}
            names = {entry.name for entry in self.session.get_inputs()}
            output = np.asarray(self.session.run(None, {k: v for k, v in inputs.items() if k in names})[0], dtype=np.float32)
            mask = inputs['attention_mask'].astype(np.float32)[..., None]
            pooled = np.sum(output * mask, axis=1) / np.maximum(mask.sum(axis=1), 1)
            norm = np.linalg.norm(pooled, axis=1, keepdims=True)
            if not np.isfinite(pooled).all() or np.any(norm == 0):
                raise ModelArtifactError('nonfinite or zero embedding')
            batches.append(cast(NDArray[np.float32], (pooled / norm).astype(np.float32)))
        return np.concatenate(batches, axis=0)
