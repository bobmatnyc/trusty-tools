"""Put scripts/ on sys.path so the tests import the `test_audit` package (#11268).

Run: python3 -I -m pytest scripts/test_audit/tests -q -p no:cacheprovider
"""
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))
