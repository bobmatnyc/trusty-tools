"""Explicit immutable reuse boundary; never construct the imported embedding adapter."""
from pathlib import Path
import sys

PREVIOUS = Path(__file__).resolve().parent.parent / 'trusty-memory-prompt-enrichment'
sys.path.append(str(PREVIOUS))
from records import (Source, Fact, Query, Evidence, Event, Gold, JSON, FixtureError,
    ProtocolError, IntegrityError, digest, eligible_facts, parse_source, obj, array,
    string, integer, keys, read_gold)
from adapters import RustHelper
from projection import Projection, graph_lookup
from retrieval import Retrieval, Packet, pack
from packet_integrity import validate_packet
from offline_encoding import load_encoding

__all__ = ['PREVIOUS', 'Source', 'Fact', 'Query', 'Evidence', 'Event', 'Gold', 'JSON',
    'FixtureError', 'ProtocolError', 'IntegrityError', 'digest', 'eligible_facts',
    'parse_source', 'obj', 'array', 'string', 'integer', 'keys', 'read_gold',
    'RustHelper', 'Projection', 'graph_lookup', 'Retrieval', 'Packet', 'pack',
    'validate_packet', 'load_encoding']
