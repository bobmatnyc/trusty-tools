"""Independent source oracle replayed from frozen input, never engine responses."""
from __future__ import annotations
from copy import deepcopy
from datetime import datetime, timezone
from typing import Any
import json

Json = dict[str, Any]
Key = tuple[str, str]


def normalize(value: Any, field: str = '') -> Any:
    if isinstance(value, dict):
        return {k:normalize(v,k) for k,v in value.items()}
    if isinstance(value, list):
        items = [normalize(v) for v in value]
        return sorted(items,key=lambda x:json.dumps(x,sort_keys=True)) if field in ('tags','aliases','links') else items
    if isinstance(value,str) and field in ('created_at','effective_at','verified_at','expires_at','valid_from','valid_to'):
        return datetime.fromisoformat(value.replace('Z','+00:00')).astimezone(timezone.utc).isoformat()
    return value


class SourceOracle:
    """Replay monotonic writes/tombstones without relying on retrieval implementation."""
    def __init__(self) -> None:
        self.sources: dict[Key,Json] = {}
        self.tombstones: dict[Key,int] = {}

    def apply(self, mutations: list[Json]) -> None:
        for mutation in mutations:
            drawer = mutation.get('drawer')
            key = (drawer['scope'],drawer['id']) if drawer else (mutation['scope'],mutation['id'])
            revision = mutation['revision']
            old = self.sources.get(key)
            floor = max(old['revision'] if old else 0,self.tombstones.get(key,0))
            if revision < floor:
                continue
            if mutation['op'] == 'remove':
                self.sources.pop(key,None); self.tombstones[key] = revision
            elif revision > self.tombstones.get(key,0):
                candidate = dict(drawer=deepcopy(drawer),revision=revision)
                if old and revision == old['revision'] and normalize(old) != normalize(candidate):
                    raise ValueError('Oracle conflicting equal revision')
                self.sources[key] = candidate
                self.tombstones.pop(key,None)

    def check(self, state: Json) -> None:
        actual = {(s['drawer']['scope'],s['drawer']['id']):s for s in state['sources']}
        tombs = {(t['scope'],t['id']):t['revision'] for t in state['tombstones']}
        if len(actual) != len(state['sources']) or normalize(actual) != normalize(self.sources):
            raise ValueError('Engine source store differs from frozen source oracle')
        if len(tombs) != len(state['tombstones']) or tombs != self.tombstones:
            raise ValueError('Engine tombstones differ from frozen source oracle')

    def drawers(self) -> dict[Key,Json]:
        return {k:dict(s['drawer'],_revision=s['revision']) for k,s in self.sources.items()}
