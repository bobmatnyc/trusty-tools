import gzip
import hashlib
import json
from pathlib import Path
import pytest
from unpack_evidence import unpack


def fixture(root: Path, name='results/result.json'):
    blob=b'{"value":1}'
    compressed=gzip.compress(blob)
    source=root/'evidence/result.gz'
    source.parent.mkdir()
    source.write_bytes(compressed)
    (root/'evidence-manifest.json').write_text(json.dumps({'files':{name:hashlib.sha256(blob).hexdigest()},
        'stored_files':{name:{'stored_path':'evidence/result.gz','stored_sha256':hashlib.sha256(compressed).hexdigest()}}}))


def test_unpack_verifies_and_is_idempotent(tmp_path):
    fixture(tmp_path)
    assert unpack(tmp_path)==1
    assert (tmp_path/'results/result.json').read_bytes()==b'{"value":1}'
    assert unpack(tmp_path)==0


def test_unpack_does_not_replace_local_results(tmp_path):
    fixture(tmp_path)
    (tmp_path/'results').mkdir()
    (tmp_path/'results/result.json').write_text('local')
    with pytest.raises(ValueError,match='Refusing'):
        unpack(tmp_path)
    assert (tmp_path/'results/result.json').read_text()=='local'


def test_unpack_rejects_escape_and_corruption(tmp_path):
    fixture(tmp_path,'../escape')
    with pytest.raises(ValueError,match='Unsafe'):
        unpack(tmp_path)
    (tmp_path/'evidence-manifest.json').unlink()
    (tmp_path/'evidence/result.gz').write_bytes(b'bad')
    # Restore a valid manifest while keeping the mismatched compressed payload.
    blob=b'x'
    (tmp_path/'evidence-manifest.json').write_text(json.dumps({'files':{'result':hashlib.sha256(blob).hexdigest()},
        'stored_files':{'result':{'stored_path':'evidence/result.gz','stored_sha256':hashlib.sha256(gzip.compress(blob)).hexdigest()}}}))
    with pytest.raises(ValueError,match='Compressed checksum'):
        unpack(tmp_path)
