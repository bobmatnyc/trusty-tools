import pytest
import requests
from offline_encoding import CACHE_KEY, load_encoding


def test_missing_and_corrupt_cache_never_download(tmp_path,monkeypatch):
    def forbidden(*args,**kwargs):
        raise AssertionError('Network attempted')
    monkeypatch.setattr(requests,'get',forbidden)
    with pytest.raises(FileNotFoundError):
        load_encoding(tmp_path)
    (tmp_path/CACHE_KEY).write_bytes(b'corrupt')
    with pytest.raises(ValueError,match='checksum'):
        load_encoding(tmp_path)


def test_pinned_encoding_known_tokens():
    encoding = load_encoding()
    assert encoding.encode('hello world') == [15339,1917]
    assert encoding.decode(encoding.encode('A café memory: 温度 24°C.')) == 'A café memory: 温度 24°C.'
