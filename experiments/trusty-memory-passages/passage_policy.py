"""Exact-source lexical selection and bounded coherent allocation for #8246.

Why: Unsupported typed queries still need verifiable note evidence.
What: Filter unchanged native candidates and register source-local passage facts.
Test: test_selection_contract, test_units_and_splitting, test_native_packets.
"""
from __future__ import annotations

from collections import Counter
from dataclasses import dataclass, replace
import hashlib
import math
from pathlib import Path
import re
import sys
from typing import Mapping

import tiktoken

sys.path.append(str(Path(__file__).resolve().parent.parent / 'trusty-memory-real-validation'))
import real_adapter  # Establish the existing immutable sibling import bridge.
from legacy import (Evidence, Source, Fact, Packet, RustHelper, JSON, IntegrityError,
                    eligible_facts, digest, validate_packet)
from contracts import Task, Selection
from retrieval import format_claims

VERSION = 'memory-passages-v1'
STOP = frozenset(('about after again also and are before can could does for from have how '
    'into its not now our please should than that the their them then there these they '
    'this those through was were what when where which who why will with would you your').split())
ABBREVIATIONS = frozenset('mr. mrs. ms. dr. prof. sr. jr. st. vs. etc. e.g. i.e.'.split())


@dataclass(frozen=True)
class Span:
    source_id: str
    start_byte: int
    end_byte: int
    source_body_sha256: str


@dataclass(frozen=True)
class Unit:
    span: Span
    section: int
    ordinal: int
    atomic: bool
    oversized: bool


@dataclass(frozen=True)
class PassageView:
    task_context: tuple[str, str, str]
    sources: tuple[Source, ...]
    units: Mapping[str, tuple[Unit, ...]]
    base: Mapping[Span, Evidence]
    preferred: Mapping[Span, Evidence]
    original_fingerprints: Mapping[str, str]
    body_digests: Mapping[str, str]
    representation: Mapping[str, JSON]


@dataclass(frozen=True)
class Lexicon:
    task_context: tuple[str, str, str]
    source_count: int
    document_frequency: Mapping[str, int]
    drawer_bodies: Mapping[str, str]


@dataclass(frozen=True)
class PackedPassages:
    packet: Packet
    expanded_candidates: tuple[Evidence, ...]
    seed_ids: Mapping[str, tuple[str, ...]]
    rejections: tuple[tuple[str, str], ...]


def _context(task: Task) -> tuple[str, str, str]:
    return task.scope, task.as_of, task.knowledge_cutoff


def _body_hash(body: str) -> str:
    return hashlib.sha256(body.encode()).hexdigest()


def _words(text: str) -> set[str]:
    return {w for w in re.findall(r'\w+', text.casefold()) if len(w) >= 3 and w not in STOP}


def _anchors(text: str) -> tuple[str, ...]:
    values = re.findall(r'(?<!\w)#\d+\b', text) + re.findall(r'`([^`\r\n]{1,128})`', text)
    values += [word.strip(',;!?()[]{}"\'') for word in text.split() if '/' in word or '::' in word]
    return tuple(sorted({a for a in values if len(a) >= 3 and any(c.isalnum() for c in a)}))


def _contains(text: str, anchor: str) -> bool:
    return re.search(r'(?<!\w)' + re.escape(anchor) + r'(?!\w)', text) is not None


def _drawers(sources: tuple[Source, ...], metadata: Mapping[str, Mapping[str, JSON]],
             task: Task) -> tuple[Source, ...]:
    if len({s.source_id for s in sources}) != len(sources):
        raise IntegrityError('duplicate source identity')
    eligible = {e.id for e in eligible_facts(sources, task.legacy_query())}
    result = []
    for source in sources:
        if source.deleted or not source.facts:
            continue
        ids = [Evidence(source, f).id for f in source.facts]
        kinds = {metadata[i]['kind'] for i in ids}
        if kinds != {'drawer_text'}:
            if kinds != {'kg_record'}:
                raise IntegrityError('invalid source kind')
            continue
        active = [i for i in ids if i in eligible]
        if not active:
            continue
        clocks = {(f.subject, f.valid_from, f.valid_to, f.single_value_slot) for f in source.facts}
        if len(active) != len(ids) or len(clocks) != 1 or any(
                f.standing or f.predicate != 'memory_text' or not f.subject.startswith('drawer:')
                or source.body.encode()[f.start_byte:f.end_byte] != f.claim.encode() for f in source.facts):
            raise IntegrityError('heterogeneous or partially ineligible drawer source')
        result.append(source)
    return tuple(result)


def build_lexicon(sources: tuple[Source, ...], metadata: Mapping[str, Mapping[str, JSON]],
                  task: Task) -> Lexicon:
    """Count eligible drawer documents once per term; test_selection_contract."""
    bodies = {s.source_id: s.body for s in _drawers(sources, metadata, task)}
    return Lexicon(_context(task), len(bodies), Counter(w for b in bodies.values() for w in _words(b)), bodies)


def select_lexical(task: Task, candidates: tuple[Evidence, ...], lexicon: Lexicon) -> Selection:
    """Return a supported ordered subset without parser or labels; test_selection_contract."""
    if _context(task) != lexicon.task_context or len(task.prompt.encode()) > 65536:
        raise IntegrityError('task context or prompt size differs')
    if len(candidates) > 20 or len({e.id for e in candidates}) != len(candidates):
        raise IntegrityError('candidate limit or duplicate')
    weights = {w: math.log((lexicon.source_count + 1) / (df + 1)) + 1
        for w in sorted(_words(task.prompt)) if 0 < (df := lexicon.document_frequency.get(w, 0)) <= lexicon.source_count * .1}
    anchors = [a for a in _anchors(task.prompt)
        if 0 < sum(_contains(b, a) for b in lexicon.drawer_bodies.values()) <= lexicon.source_count * .1]
    selected: list[Evidence] = []
    reasons: list[tuple[str, str]] = []
    for e in candidates:
        if e.source.source_id not in lexicon.drawer_bodies:
            reasons.append((e.id, 'kg_record'))
            continue
        if e.source.body != lexicon.drawer_bodies[e.source.source_id] or e.fact not in e.source.facts:
            raise IntegrityError('candidate source or fact differs')
        matched = _words(e.fact.claim) & weights.keys()
        supported = (len(matched) >= 2 and math.fsum(weights[w] for w in sorted(matched)) >=
                     .25 * math.fsum(weights.values()))
        supported = supported or any(_contains(e.fact.claim, a) for a in anchors)
        if supported and len(selected) < 8:
            selected.append(e)
        else:
            reasons.append((e.id, 'seed_cap' if supported else 'below_threshold'))
    reason = ('selected' if selected else 'no_candidates' if not candidates else
              'empty_query' if not weights and not anchors else 'below_threshold')
    return Selection(tuple(selected), rejections=tuple(reasons) + (('query', reason),))


def _fits(text: str, encoding: tiktoken.Encoding) -> bool:
    return len(text.encode()) <= 2048 and len(encoding.encode(text, disallowed_special=())) <= 160


def _fragments(text: str, delimiter: str) -> list[str]:
    boundaries = [0]
    ticks = 0
    for match in re.finditer(r'`+|[.!?;](?=\s)', text):
        token = match.group()
        if token.startswith('`'):
            ticks = len(token) if not ticks else 0 if ticks == len(token) else ticks
            continue
        if ticks or token not in delimiter:
            continue
        previous = text[:match.end()].split()[-1].casefold()
        if token == '.' and (previous in ABBREVIATIONS or any(c.isdigit() for c in previous)
                or any(c in previous[:-1] for c in ('/', '\\', '.')) or '::' in previous):
            continue
        end = match.end()
        while end < len(text) and text[end].isspace():
            end += 1
        boundaries.append(end)
    return [text[a:b] for a, b in zip(boundaries, boundaries[1:] + [len(text)]) if a < b]


def _units(source: Source, encoding: tiktoken.Encoding) -> tuple[Unit, ...]:
    chunks: list[tuple[str, int, bool]] = []
    pending = ''
    section = 0
    fence = ''
    for line in source.body.splitlines(keepends=True):
        if fence:
            pending += line
            if re.fullmatch(r'\s*' + re.escape(fence[0]) + '{' + str(len(fence)) + r',}\s*', line):
                chunks.append((pending, section, True))
                pending, fence = '', ''
            continue
        marker = re.match(r'^\s*(`{3,}|~{3,})', line)
        heading = re.match(r'^#{1,6}\s', line)
        item = re.match(r'^\s*(?:[-*+]|\d+[.)])\s', line)
        if marker or heading or item or not line.strip():
            if pending:
                chunks.append((pending, section, False))
                pending = ''
        if heading:
            section += 1
            chunks.append((line, section, False))
        elif not line.strip():
            chunks.append((line, section, False))
        else:
            pending += line
            if marker:
                fence = marker.group(1)
    if pending:
        chunks.append((pending, section, bool(fence)))
    result: list[Unit] = []
    position = 0
    sha = _body_hash(source.body)
    for text, group, atomic in chunks:
        parts = [text]
        if not atomic and not _fits(text, encoding):
            parts = [part for sentence in _fragments(text, '.!?')
                for part in ([sentence] if _fits(sentence, encoding) else _fragments(sentence, ';'))]
        for part in parts:
            end = position + len(part.encode())
            if part.strip():
                result.append(Unit(Span(source.source_id, position, end, sha), group,
                    len(result), atomic, not _fits(part, encoding)))
            position = end
    return tuple(result)


def derive_passages(sources: tuple[Source, ...], metadata: Mapping[str, Mapping[str, JSON]],
                    task: Task, encoding: tiktoken.Encoding) -> PassageView:
    """Register bounded whole units and windows; test_units_and_splitting."""
    drawers = _drawers(sources, metadata, task)
    derived: list[Source] = []
    units_by_source: dict[str, tuple[Unit, ...]] = {}
    base: dict[Span, Evidence] = {}
    preferred: dict[Span, Evidence] = {}
    stats = Counter({'notes': len(drawers), 'bytes': sum(len(s.body.encode()) for s in drawers)})
    for source in drawers:
        units = units_by_source[source.source_id] = _units(source, encoding)
        body = source.body.encode()
        facts: dict[str, Fact] = {}
        choices: dict[Span, tuple[Fact, Fact]] = {}
        for i, unit in enumerate(units):
            span = unit.span
            if unit.oversized:
                stats['oversized_units'] += 1
                stats['oversized_fences'] += unit.atomic
                stats['oversized_bytes'] += span.end_byte - span.start_byte
                continue
            alternatives = [(max(0, i-1), min(len(units)-1, i+1)), (i, min(len(units)-1, i+1)),
                            (max(0, i-1), i), (i, i)]
            window = span
            for left, right in alternatives:
                start, end = units[left].span.start_byte, units[right].span.end_byte
                if all(u.section == unit.section and not u.oversized for u in units[left:right+1]) and _fits(body[start:end].decode(), encoding):
                    window = Span(span.source_id, start, end, span.source_body_sha256)
                    break
            pair = []
            for choice in (span, window):
                claim = body[choice.start_byte:choice.end_byte].decode()
                fid = 'p' + digest((VERSION, choice.source_body_sha256, source.revision,
                                   choice.start_byte, choice.end_byte))[:16]
                fact = replace(source.facts[0], fact_id=fid, claim=claim, object=claim,
                               start_byte=choice.start_byte, end_byte=choice.end_byte)
                if fid in facts and facts[fid] != fact:
                    raise IntegrityError('passage ID collision')
                facts[fid] = fact
                pair.append(fact)
            choices[span] = pair[0], pair[1]
        registered = replace(source, facts=tuple(facts.values()))
        derived.append(registered)
        for span, (unit_fact, window_fact) in choices.items():
            base[span], preferred[span] = Evidence(registered, unit_fact), Evidence(registered, window_fact)
        representable = sum(u.span.end_byte-u.span.start_byte for u in units if not u.oversized)
        represented = _union_length([(f.start_byte, f.end_byte) for f in facts.values()])
        stats['base_representable_bytes'] += representable
        stats['representable_bytes'] += represented
        stats['blank_gap_bytes'] += len(body)-sum(u.span.end_byte-u.span.start_byte for u in units)
        stats['notes_any_representation'] += bool(facts)
        stats['notes_full_representation'] += represented == len(body)
        stats['notes_with_oversized_loss'] += any(u.oversized for u in units)
    representation: dict[str, JSON] = dict(stats)
    representation['representable_byte_fraction'] = stats['representable_bytes']/stats['bytes'] if stats['bytes'] else None
    representation['representable_note_fraction'] = stats['notes_any_representation']/stats['notes'] if stats['notes'] else None
    return PassageView(_context(task), tuple(derived), units_by_source, base, preferred,
        {s.source_id: s.fingerprint for s in drawers}, {s.source_id: _body_hash(s.body) for s in drawers}, representation)


def _union_length(intervals: list[tuple[int, int]]) -> int:
    from real_evaluate import merge_intervals
    return sum(b-a for a, b in merge_intervals(intervals))


def _overlap(a: Evidence, b: Evidence) -> bool:
    return a.source.key == b.source.key and a.fact.start_byte < b.fact.end_byte and b.fact.start_byte < a.fact.end_byte


def pack_passages(task: Task, selection: Selection, view: PassageView, budget: int,
                  helper: RustHelper, encoding: tiktoken.Encoding) -> PackedPassages:
    """Admit whole registered choices under full rendered budgets; test_native_packets."""
    if _context(task) != view.task_context or budget not in (128, 256):
        raise IntegrityError('passage context or budget differs')
    selected: list[Evidence] = []
    expanded: dict[str, Evidence] = {}
    reasons = list(selection.rejections)
    text = ''
    for seed in selection.evidence:
        if seed.source.fingerprint != view.original_fingerprints.get(seed.source.source_id) or seed.fact not in seed.source.facts:
            raise IntegrityError('seed differs from original authority')
        units = [u for u in view.units[seed.source.source_id]
                 if u.span.start_byte < seed.fact.end_byte and seed.fact.start_byte < u.span.end_byte]
        if not units:
            reasons.append((seed.id, 'no_unit'))
        for unit in units:
            if unit.oversized:
                reasons.append((seed.id, 'oversized_unit'))
                continue
            choices = tuple(dict.fromkeys((view.preferred[unit.span].id, view.base[unit.span].id)))
            available = {e.id: e for e in (view.preferred[unit.span], view.base[unit.span])}
            expanded.update(available)
            for identity in choices:
                e = available[identity]
                if any(_overlap(e, prior) for prior in selected):
                    reasons.append((identity, 'overlap'))
                    continue
                candidate = format_claims(selected + [e], helper)
                if len(encoding.encode(candidate, disallowed_special=())) > budget:
                    reasons.append((identity, 'budget'))
                    continue
                selected.append(e)
                text = candidate
                break
    packet = Packet(text, len(encoding.encode(text, disallowed_special=())), selected, set(), 0,
                    [i for i, reason in reasons if reason == 'budget'], {e.id: e.fact.claim for e in selected}, {})
    return PackedPassages(packet, tuple(expanded.values()),
        {e.id: tuple(s.id for s in selection.evidence if _overlap(e, s)) for e in selected}, tuple(reasons))


def validate_passages(result: PackedPassages, originals: tuple[Source, ...], view: PassageView,
                      selection: Selection, task: Task, budget: int, helper: RustHelper,
                      encoding: tiktoken.Encoding) -> None:
    """Keep complete-corpus eligibility; test_validator_rejects_globally_superseded_seed."""
    eligible = {e.id: e for e in eligible_facts(originals, task.legacy_query())}
    wanted = {s.source.source_id for s in selection.evidence}
    relevant = tuple(s for s in originals if s.source_id in wanted)
    if {s.source_id for s in relevant} != wanted:
        raise IntegrityError('missing original seed source')
    if any(eligible.get(e.id) != e for e in selection.evidence):
        raise IntegrityError('ineligible or forged seed fact')
    if any(Evidence(s, f).id not in eligible for s in relevant for f in s.facts):
        raise IntegrityError('partially ineligible referenced source')
    metadata: dict[str, Mapping[str, JSON]] = {Evidence(s, f).id: {'kind': 'drawer_text'}
                                              for s in relevant for f in s.facts}
    rebuilt = derive_passages(relevant, metadata, task, encoding)
    for sid in wanted:
        if (view.original_fingerprints.get(sid) != rebuilt.original_fingerprints.get(sid)
                or view.body_digests.get(sid) != rebuilt.body_digests.get(sid)):
            raise IntegrityError('original source digest differs')
    expected = pack_passages(task, selection, rebuilt, budget, helper, encoding)
    if result != expected:
        raise IntegrityError('passage registration, expansion or packet differs')
    validate_packet(result.packet, rebuilt.sources, task.legacy_query(), treatment='bm25',
                    budget=budget, helper=helper, encoding=encoding)
