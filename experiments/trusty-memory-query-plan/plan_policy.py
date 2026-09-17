"""Bounded grammar preserves references and relation binding; no fixture metadata enters.

Why: #8246 exposed flattened relations and negation scope errors.
What: Parse the frozen interface grammar into immutable requests.
Test: test_positive_requests_and_exclusions, test_reference_boundaries_and_independent_binding.
"""
from __future__ import annotations
from dataclasses import replace
import re
from typing import cast
import plan_bridge
from contracts import Task
from relevance_index import RelevanceIndex
from legacy import IntegrityError
from plan_contracts import Span, EntityRef, Step, Request, Plan, Predicate, ParseStatus
from plan_index import validate_context

VOCAB: dict[Predicate, tuple[str, ...]] = {
    'owned_by': ('owner', 'accountable owner', 'ownership', 'owned by'),
    'maintained_by': ('maintainer', 'maintenance team', 'maintained by'),
    'located_at': ('location', 'site', 'address', 'located at'),
    'access_code': ('access code', 'entry code', 'door code'),
    'rationale': ('reason', 'rationale', 'original purpose'),
    'release_rule': ('release rule', 'release prerequisite', 'release prerequisites', 'approval prerequisite'),
    'release_prohibition': ('release prohibition', 'explicit prohibition', 'prohibition on releasing'),
    'uses_channel': ('channel', 'notification channel'),
    'depends_on': ('dependency', 'dependencies', 'immediate dependencies', 'depends on'),
    'uses': ('asset', 'assets', 'used asset', 'used assets', 'uses'),
    'escalates_to': ('escalation', 'escalation target', 'escalates to'),
    'contact_window': ('contact window', 'availability', 'contact hours'),
    'is_alias_for': ('alias', 'alias meanings', 'interpretations', 'alternatives'),
}
PHRASES = {phrase: predicate for predicate, phrases in VOCAB.items() for phrase in phrases}
COMMAND = r'(?:(?:please|give|show|list|identify|find|name|compare|what is|what are|the|both|all)\s+)'
PRESENTATION = r'\b(?:once|without duplicates|do not pick a single referent|not the shorter similarly named (?:asset|annex))\b'
NAME = r'[A-Z][\w-]*(?:\s+[A-Z][\w-]*)*'
NONNAME = set(('please give show list identify find name compare what where who why do the both all its '
    'utc after before named incoming services').split()) | {word for phrase in PHRASES for word in phrase.split()}

def normalize(text: str) -> str:
    return ' '.join(text.casefold().split()).strip(' ,')

def strip_prefix(text: str) -> str:
    return re.sub(r'^' + COMMAND + '+', '', text).strip()

def references(text: str, offset: int, index: RelevanceIndex) -> tuple[str, tuple[EntityRef, ...]]:
    matches: list[tuple[int, int, str]] = []
    for m in re.finditer(r'"([^"\n]+)"|(?<!\w)\x27([^\x27\n]+)\x27', text):
        matches.append((m.start(), m.end(), m.group(1) or m.group(2)))
    for m in re.finditer(NAME, text):
        if any(m.start() < b and a < m.end() for a, b, _ in matches):
            continue
        parts = m.group().split()
        while parts and not text[:m.start()].strip() and parts[0].casefold() in NONNAME and not any(
                n.casefold() == ' '.join(parts).casefold() for n in index.names):
            parts.pop(0)
        if not parts or (all(p.casefold() in NONNAME for p in parts) and not any(
                n.casefold() == ' '.join(parts).casefold() for n in index.names)):
            continue
        value = ' '.join(parts)
        start = m.end()-len(value)
        matches.append((start, m.end(), value))
    refs: list[EntityRef] = []
    chunks: list[str] = []
    previous = 0
    for start, end, value in sorted(matches):
        names = tuple(n for n in index.names if n.casefold() == value.casefold())
        targets = tuple(sorted({target for n in names for target in index.aliases.get(n, (n,))}))
        aliases = tuple(sorted({e.id for n in names for e in index.alias_evidence.get(n, ())}))
        status: ParseStatus = ('unresolved_entity' if not targets else
            ('ambiguous_entity' if len(targets) > 1 else 'ready'))
        refs.append(EntityRef(Span(offset+start, offset+end, text[start:end]), targets, aliases, status))
        chunks.extend((text[previous:start], f' @{len(refs)-1} '))
        previous = end
    chunks.append(text[previous:])
    return normalize(''.join(chunks)), tuple(refs)

def relation(text: str) -> Step | None:
    text = strip_prefix(text)
    incoming = text.startswith('incoming ')
    if incoming:
        text = text[9:]
    predicate = PHRASES.get(text)
    return Step(predicate, 'in' if incoming else 'out') if predicate else None

def expression(text: str, depth: int = 0) -> list[tuple[tuple[int, ...], tuple[Step, ...]]]:
    """Parse only full expressions; unknown tails cannot disappear behind a keyword."""
    if depth > 2:
        return []
    text = strip_prefix(text)
    if re.fullmatch(r'@\d+(?:\s+and\s+@\d+)*', text):
        return [(tuple(int(v) for v in re.findall(r'@(\d+)', text)), ())]
    m = re.fullmatch(r'(@\d+)\s*[\x27’]s\s+(.+)', text)
    if m:
        return expression(m[2]+' of '+m[1], depth)
    for pattern, phrase in ((r'where is (.+)', 'location'), (r'who owns (.+)', 'owner'),
                             (r'who maintains (.+)', 'maintainer'), (r'why was (.+) created', 'rationale'),
                             (r'what could (.+) refer to', 'alias meanings')):
        m = re.fullmatch(pattern, text)
        if m:
            return expression(phrase+' of '+m[1], depth)
    m = re.fullmatch(r'services (maintained by|owned by|that depend on) (.+)', text)
    if m:
        inverse: dict[str, Predicate] = {'maintained by': 'maintained_by', 'owned by': 'owned_by',
            'that depend on': 'depends_on'}
        pred = inverse[m[1]]
        bases = expression(m[2], depth+1)
        return [(roots, (*steps, Step(pred, 'in'))) for roots, steps in bases]
    m = re.fullmatch(r'prohibition on releasing (.+)', text)
    if m:
        return expression('release prohibition of '+m[1], depth)
    # Prefer a complete property prefix; "and" between requests is handled only after this fails.
    for match in re.finditer(r'\s+of\s+', text):
        properties = [relation(s) for s in text[:match.start()].split(' and ')]
        if not properties or any(p is None for p in properties):
            continue
        bases = expression(text[match.end():], depth+1)
        if bases:
            return [(roots, (*steps, cast(Step, prop))) for prop in properties for roots, steps in bases
                if len(steps) < 2]
    for match in re.finditer(r'\s+and\s+', text):
        left, right = expression(text[:match.start()], depth+1), expression(text[match.end():], depth+1)
        if left and right and all(steps for _, steps in (*left, *right)):
            return left+right
    return []

def parse_plan(task: Task, index: RelevanceIndex) -> Plan:
    """Pre: nonempty prompt and scoped eligible index. Post: preserve every substantive clause."""
    validate_context(task, index)
    if not task.prompt.strip():
        raise IntegrityError('empty prompt')
    requests: list[Request] = []
    diagnostics: list[tuple[Span, str]] = []
    previous: tuple[EntityRef, ...] = ()
    previous_group: tuple[int, ...] = ()
    for match in re.finditer(r'[^;.!?]+', task.prompt):
        original = match.group()
        if not original.strip():
            continue
        span = Span(match.start(), match.end(), original)
        if re.search(r'@\d+', original):
            requests.append(Request(span, (), (), status='unsupported'))
            diagnostics.append((span, 'reserved_reference_marker'))
            previous, previous_group = (), (len(requests)-1,)
            continue
        text, refs = references(original, match.start(), index)
        if refs:
            previous = ()
        text = re.sub(PRESENTATION, '', text, flags=re.IGNORECASE).strip(' ,')
        if not text:
            continue
        excluded: tuple[Predicate, ...] = ()
        modifier = re.fullmatch(r'(?:omit (.+)|(.+) would not answer this)', strip_prefix(text))
        if modifier:
            output = relation(modifier[1] or modifier[2])
            if output and previous_group:
                for number in previous_group:
                    request = requests[number]
                    requests[number] = replace(request, excluded_outputs=(*request.excluded_outputs, output.predicate))
                continue
        m = re.search(r',\s*not (.+)$', text)
        if m:
            output = relation(m[1])
            if output:
                excluded, text = (output.predicate,), text[:m.start()]
        forbidden_dependencies = 'without following dependencies' in text
        text = text.replace('without following dependencies', '').strip()
        target_refs: tuple[EntityRef, ...] = ()
        m = re.search(r'\s+(?:excluding|except)\s+(@\d+(?:\s+and\s+@\d+)*)$', text)
        if m:
            target_refs = tuple(refs[int(v)] for v in re.findall(r'@(\d+)', m[1]))
            text = text[:m.start()]
        qualifier: str | None = None
        m = re.search(r'\s+(after ([01]\d|2[0-3]):[0-5]\d utc)$', text)
        if m:
            qualifier, text = m[1], text[:m.start()]
        text = re.sub(r'\bnamed\s+(?=@)', '', text)
        if re.fullmatch(r'its .+', text) and not refs and len(previous) == 1:
            refs = previous
            text = text[4:]+' of @0'
        parsed = expression(text)
        previous_group = (len(requests),)
        if not parsed or any(not steps for _, steps in parsed) or re.search(r'\b(not|never|suppose|hypothetical|if)\b', text):
            requests.append(Request(span, refs, (), status='unsupported'))
            diagnostics.append((span, 'unsupported'))
            continue
        previous_group = tuple(range(len(requests), len(requests)+len(parsed)))
        for roots, steps in parsed:
            selected = tuple(refs[i] for i in roots)
            enumerating = steps[0].predicate == 'is_alias_for'
            if enumerating:
                selected = tuple(replace(ref, targets=tuple(sorted({index.evidence[i].fact.subject for i in ref.alias_ids})),
                    alias_ids=(), status='ready') if ref.alias_ids else ref for ref in selected)
            status: ParseStatus = next((r.status for r in (*selected, *target_refs) if r.status != 'ready'), 'ready')
            if qualifier:
                if steps[-1].predicate != 'release_prohibition':
                    status = 'unsupported'
                steps = (*steps[:-1], replace(steps[-1], qualifier=qualifier))
            if forbidden_dependencies and any(s.predicate == 'depends_on' for s in steps):
                status = 'unsupported'
            requests.append(Request(span, selected, steps, target_refs, excluded, enumerating, status))
            if status != 'ready':
                diagnostics.append((span, status))
            if selected and all(r.status == 'ready' for r in selected):
                previous = selected
    return Plan(tuple(requests), tuple(diagnostics))
