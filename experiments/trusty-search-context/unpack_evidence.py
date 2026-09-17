"""Verify and unpack frozen evidence without replacing local experiment results."""
from __future__ import annotations
import gzip
import hashlib
import json
from pathlib import Path


def unpack(root: Path) -> int:
    """Validate all hashes/paths first; refuse changed existing output files."""
    root = root.resolve()
    manifest = json.loads((root / 'evidence-manifest.json').read_text())
    pending: list[tuple[Path, bytes]] = []
    for name, expected in manifest['files'].items():
        relative = Path(name)
        stored = Path(manifest['stored_files'][name]['stored_path'])
        if any(p.is_absolute() or '..' in p.parts for p in (relative, stored)):
            raise ValueError('Unsafe evidence path')
        target, source = root / relative, root / stored
        if any(root not in p.resolve().parents for p in (target, source)):
            raise ValueError('Evidence path escapes experiment directory')
        compressed = source.read_bytes()
        if hashlib.sha256(compressed).hexdigest() != manifest['stored_files'][name]['stored_sha256']:
            raise ValueError(f'Compressed checksum mismatch: {name}')
        data = gzip.decompress(compressed)
        if hashlib.sha256(data).hexdigest() != expected:
            raise ValueError(f'Evidence checksum mismatch: {name}')
        if target.exists():
            if target.read_bytes() != data:
                raise ValueError(f'Refusing to replace local results: {name}')
        else:
            pending.append((target, data))
    for target, data in pending:
        target.parent.mkdir(parents=True, exist_ok=True)
        with target.open('xb') as stream:
            stream.write(data)
    return len(pending)


if __name__ == '__main__':
    print(f'Unpacked {unpack(Path(__file__).resolve().parent)} verified evidence files')
