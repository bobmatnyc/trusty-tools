"""Frozen text-only intent lexicon. No fixture queries, categories, or gold imports."""
from __future__ import annotations
import re
from typing import TYPE_CHECKING
from contracts import Task, Demand
if TYPE_CHECKING:
    from relevance_index import RelevanceIndex

LEXICON: dict[str, tuple[str, ...]] = {
    'owner': ('owner', 'owned by', 'ownership', 'maintains', 'maintainer', 'maintained by', 'responsible for'),
    'location': ('where', 'location', 'located', 'address', 'floor'),
    'access': ('access code', 'entry code', 'door code', 'passcode', 'combination'),
    'reason': ('why', 'reason', 'rationale', 'because'),
    'release': ('release', 'deployment', 'deploy', 'approval', 'rollout'),
    'channel': ('channel', 'notify', 'notification', 'message', 'messaging'),
    'dependency': ('depends on', 'dependency', 'dependencies', 'relies on', 'requires'),
    'reverse_dependency': ('depends on this', 'depend on this', 'dependent', 'dependents', 'used by', 'relies on this'),
    'escalation': ('escalate', 'escalation', 'escalates to', 'backup contact'),
    'contact': ('contact', 'contact window', 'availability', 'hours', 'reachable', 'reach'),
    'alias': ('alias', 'aliases', 'alternatives', 'could refer to', 'could mean', 'stands for'),
}
PREDICATES = {'owner': ('owned_by', 'maintained_by'), 'location': ('located_at',),
    'access': ('access_code',), 'reason': ('rationale',), 'release': ('release_rule',),
    'channel': ('uses_channel',), 'dependency': ('depends_on', 'uses'),
    'reverse_dependency': ('depends_on', 'uses'), 'escalation': ('escalates_to',),
    'contact': ('contact_window',), 'alias': ('is_alias_for',)}
STOP = frozenset('a an and are as at be by can do for from have how i in is it me of on or please the this to was what when which who with would you'.split())
NEGATION = {'not', 'never', 'without'}


def words(text: str) -> tuple[str, ...]:
    return tuple(re.findall(r'\w+', text.casefold()))


def resolve_entities(text: str, index: RelevanceIndex) -> tuple[tuple[str, ...], tuple[str, ...], bool]:
    """Longest exact name spans block shorter aliases before ambiguity resolution."""
    matches: list[tuple[int, int, str]] = []
    lowered = text.casefold()
    for name in index.names:
        for match in re.finditer(r'(?<![\w-])' + re.escape(name.casefold()) + r'(?![\w-])', lowered):
            matches.append((match.start(), match.end(), name))
    kept: list[tuple[int, int, str]] = []
    for item in sorted(matches, key=lambda v: (-(v[1] - v[0]), v[0], v[2])):
        if any(item[0] < old[1] and old[0] < item[1] and item[:2] != old[:2] for old in kept):
            continue
        kept.append(item)
    entities: list[str] = []
    alias_ids: list[str] = []
    ambiguous = False
    for start, end, name in sorted(kept):
        equal = {candidate for a, b, candidate in kept if (a, b) == (start, end)}
        targets = {target for candidate in equal for target in index.aliases.get(candidate, (candidate,))}
        alias_ids.extend(e.id for candidate in equal for e in index.alias_evidence.get(candidate, ()))
        if len(targets) != 1:
            ambiguous = True
            continue
        target = next(iter(targets))
        if target not in entities:
            entities.append(target)
    return tuple(entities[:3]), tuple(sorted(set(alias_ids))), ambiguous


def parse_intents(task: Task, index: RelevanceIndex) -> tuple[Demand, ...]:
    demands: list[Demand] = []
    previous: tuple[str, ...] = ()
    previous_aliases: tuple[str, ...] = ()
    for clause in re.split(r'[;.!?]+|\bbut\b', task.prompt, flags=re.IGNORECASE):
        tokens = words(clause)
        if not tokens:
            continue
        entities, aliases, ambiguous = resolve_entities(clause, index)
        if not entities and not ambiguous and not aliases:
            entities, aliases = previous, previous_aliases
        if entities and not ambiguous:
            previous, previous_aliases = entities, aliases
        hypothetical = 'hypothetical' in tokens or 'suppose' in tokens or any(tokens[i:i+2] == ('what', 'if') for i in range(len(tokens)))
        matches: list[tuple[int, int, str]] = []
        for intent, phrases in LEXICON.items():
            for phrase in phrases:
                part = words(phrase)
                for i in range(len(tokens) - len(part) + 1):
                    if tokens[i:i+len(part)] == part:
                        matches.append((i, i+len(part), intent))
        retained: list[tuple[int, int, str]] = []
        for item in sorted(matches, key=lambda m: (-(m[1]-m[0]), m[0], m[2])):
            if any(item[0] < old[1] and old[0] < item[1] and (item[2] == old[2] or
                (item[2], old[2]) in {('dependency', 'reverse_dependency'), ('contact', 'escalation')}) for old in retained):
                continue
            retained.append(item)
        seen = set()
        for begin, end, intent in sorted(retained):
            if intent in seen:
                continue
            seen.add(intent)
            status = 'hypothetical' if hypothetical else ('negated_demand' if NEGATION & set(tokens[max(0, begin-3):begin])
                else ('ambiguous_entity' if ambiguous and intent != 'alias' else ('ready' if entities or (intent == 'alias' and aliases) else 'unresolved_entity')))
            demands.append(Demand(intent, entities, status, aliases, clause))
        if not retained:
            status = 'hypothetical' if hypothetical else ('negated_demand' if NEGATION & set(tokens) else
                ('ambiguous_entity' if ambiguous else ('unknown_intent' if entities else 'unresolved_entity')))
            demands.append(Demand('unknown', entities, status, aliases, clause))
    return tuple(demands[:3])
