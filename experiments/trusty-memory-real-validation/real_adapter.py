"""Private current-corpus adaptation; no retrieval or semantic extraction.

Why: #8246 needs real note evidence without pretending graph rows are note excerpts.
What: Preserve source bytes, native predicates, raw times, and frozen prompt locators.
Test: test_chunks_and_snapshot_integrity, test_private_outputs_and_sample_digest.
"""
from __future__ import annotations
from dataclasses import asdict, dataclass
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import sys
from typing import Mapping, get_args
import tiktoken

sys.path.append(str(Path(__file__).resolve().parent.parent / 'trusty-memory-query-plan'))
import plan_bridge
from legacy import Source, Fact, Evidence, JSON, IntegrityError, obj, array, string, integer, digest
from plan_contracts import Predicate

@dataclass(frozen=True)
class AdaptedCorpus:
    sources: tuple[Source, ...]
    evidence_metadata: Mapping[str, Mapping[str, JSON]]
    counts: Mapping[str, int]
    as_of: str

def file_digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()

def read_json(path: Path) -> dict[str, JSON]:
    return obj(json.loads(path.read_text()))

def private_dir(path: Path) -> Path:
    resolved = path.resolve()
    if any((parent / '.git').exists() for parent in (resolved, *resolved.parents)):
        raise IntegrityError('private output is inside a Git worktree')
    resolved.mkdir(mode=0o700, parents=False, exist_ok=False)
    return resolved

def write_private(path: Path, value: object) -> None:
    if any((parent / '.git').exists() for parent in path.resolve().parents):
        raise IntegrityError('private output is inside a Git worktree')
    payload = json.dumps(value, ensure_ascii=False, sort_keys=True).encode()
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    try:
        with os.fdopen(fd, 'wb') as handle:
            handle.write(payload)
    except BaseException:
        path.unlink()
        raise

def read_snapshot(path: Path, max_rows: int) -> dict[str, JSON]:
    data = read_json(path)
    if data.get('version') != 'memory-real-snapshot-v1' or max_rows <= 0:
        raise IntegrityError('invalid snapshot version or limit')
    total = 0
    for table in ('drawers', 'triples'):
        rows = array(data[table])
        counts = obj(obj(data['counts'])[table])
        if counts != {'read':len(rows), 'decoded':len(rows), 'errors':0}:
            raise IntegrityError('snapshot count mismatch')
        keys = [string(obj(row)['key_hex']) for row in rows]
        if len(set(keys)) != len(keys):
            raise IntegrityError('duplicate snapshot key')
        for key in keys:
            if not key or len(key) % 2 or any(c not in '0123456789abcdef' for c in key):
                raise IntegrityError('invalid snapshot key')
        total += len(rows)
    if total > max_rows:
        raise IntegrityError('snapshot row limit exceeded')
    integer(data['captured_at_ms'])
    return data

def chunks(text: str, encoding: tiktoken.Encoding, limit: int = 80) -> tuple[tuple[int, int, str], ...]:
    """Return disjoint exact UTF-8 spans; every chunk stays within the token cap."""
    if limit < 4:
        raise IntegrityError('chunk token limit must be at least four')
    tokens = encoding.encode(text, disallowed_special=())
    result: list[tuple[int, int, str]] = []
    position = byte_start = 0
    while position < len(tokens):
        end = min(len(tokens), position + limit)
        while True:
            payload = b''.join(encoding.decode_single_token_bytes(t) for t in tokens[position:end])
            try:
                claim = payload.decode('utf-8')
            except UnicodeDecodeError:
                end -= 1
                continue
            if len(encoding.encode(claim, disallowed_special=())) <= limit:
                break
            end -= 1
        if claim.strip():
            result.append((byte_start, byte_start + len(payload), claim))
        byte_start += len(payload)
        position = end
    return tuple(result)

def adapt_snapshot(data: Mapping[str, JSON], scope: str, encoding: tiktoken.Encoding) -> AdaptedCorpus:
    now = integer(data['captured_at_ms'])
    stamp = datetime.fromtimestamp(now // 1000, timezone.utc).strftime('%Y-%m-%dT%H:%M:%SZ')
    sources: list[Source] = []
    metadata: dict[str, Mapping[str, JSON]] = {}
    counts = {'expired_drawers':0, 'empty_drawers':0, 'ineligible_triples':0, 'drawer_chunks':0,
              'graph_facts':0, 'object_entities':0, 'object_literals':0, 'content_tokens':0,
              'supported_graph_facts':0, 'unsupported_graph_facts':0, 'graph_with_provenance':0}
    eligible: list[dict[str, JSON]] = []
    for value in array(data['triples']):
        row = obj(value)
        end = row['valid_to_ms']
        if integer(row['valid_from_ms']) > now or (end is not None and now >= integer(end)):
            counts['ineligible_triples'] += 1
        else:
            if row['row_kind'] not in ('active', 'history'):
                raise IntegrityError('invalid graph row kind')
            eligible.append(row)
    subjects = {string(row['subject']) for row in eligible}
    for value in array(data['drawers']):
        row = obj(value)
        record = obj(row['record'])
        expiry = record['expires_at_ms']
        if expiry is not None and now >= integer(expiry):
            counts['expired_drawers'] += 1
            continue
        body = record['content']
        if not isinstance(body, str):
            raise IntegrityError('drawer content must be a string')
        spans = chunks(body, encoding)
        if not spans:
            counts['empty_drawers'] += 1
            continue
        sid = 'd' + digest(row['key_hex'])[:12]
        facts = tuple(Fact(str(i), 'drawer:' + string(row['key_hex']), 'memory_text', claim,
            None, claim, start, end, stamp, None, None, False) for i, (start, end, claim) in enumerate(spans))
        source = Source(sid, scope, 1, sid, body, stamp, None, None, False, facts)
        sources.append(source)
        for fact in facts:
            metadata[Evidence(source, fact).id] = {'kind':'drawer_text', 'source_id':sid,
                'key_hex':row['key_hex'], 'start_byte':fact.start_byte, 'end_byte':fact.end_byte,
                'decode_version':row.get('decode_version', 'unknown'), 'absent_fields':row.get('absent_fields', []),
                'record':{key:value for key, value in record.items() if key != 'content'}}
        counts['drawer_chunks'] += len(facts)
        counts['content_tokens'] += len(encoding.encode(body, disallowed_special=()))
    for row in eligible:
        subject, predicate, value = (string(row[key]) for key in ('subject', 'predicate', 'object'))
        body = f'{subject} {predicate} {value}'
        sid = 'g' + digest(row['key_hex'])[:12]
        entity = value if value in subjects else None
        fact = Fact('0', subject, predicate, value, entity, body, 0, len(body.encode()), stamp, None, None, False)
        source = Source(sid, scope, 1, sid, body, stamp, None, None, False, (fact,))
        sources.append(source)
        metadata[Evidence(source, fact).id] = {'kind':'kg_record', 'source_id':sid, 'record':row,
            'start_byte':0, 'end_byte':len(body.encode()), 'drawer_provenance':'unresolved'}
        counts['graph_facts'] += 1
        counts['supported_graph_facts' if predicate in get_args(Predicate) else 'unsupported_graph_facts'] += 1
        counts['graph_with_provenance'] += row['provenance'] is not None
        counts['object_entities' if entity else 'object_literals'] += 1
        counts['content_tokens'] += len(encoding.encode(body, disallowed_special=()))
    if len({source.key for source in sources}) != len(sources):
        raise IntegrityError('source ID collision')
    return AdaptedCorpus(tuple(sources), metadata, counts, stamp)

def read_frozen_sample(path: Path, expected_digest: str) -> list[dict[str, JSON]]:
    if file_digest(path) != expected_digest:
        raise IntegrityError('frozen sample digest mismatch')
    data = read_json(path)
    queries = [obj(value) for value in array(data['queries'])]
    if len(queries) != 32 or len({string(row['id']) for row in queries}) != 32:
        raise IntegrityError('frozen sample must contain 32 unique IDs')
    log_hashes = obj(data['log_hashes'])
    for row in queries:
        for key in ('prompt', 'logged_at', 'log_file', 'palace', 'id'):
            string(row[key])
        integer(row['line'])
        string(log_hashes[string(row['log_file'])])
    return queries

def prepare(snapshot: Path, prompts: Path, sample_digest: str, output: Path,
            scope: str, encoding: tiktoken.Encoding, max_rows: int) -> None:
    data = read_snapshot(snapshot, max_rows)
    queries = read_frozen_sample(prompts, sample_digest)
    if any(row['palace'] != scope for row in queries):
        raise IntegrityError('sample palace differs from corpus scope')
    corpus = adapt_snapshot(data, scope, encoding)
    destination = private_dir(output)
    write_private(destination / 'prepared.json', {'version':'memory-real-prepared-v1',
        'sources':[asdict(source) for source in corpus.sources], 'metadata':corpus.evidence_metadata,
        'counts':corpus.counts, 'as_of':corpus.as_of, 'scope':scope, 'queries':queries,
        'log_hashes':read_json(prompts)['log_hashes'], 'snapshot_sha256':file_digest(snapshot),
        'drawer_decode_versions':data.get('drawer_decode_versions', {}),
        'sample_sha256':sample_digest})
