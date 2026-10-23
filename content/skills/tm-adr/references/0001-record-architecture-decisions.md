# 0001. Record architecture decisions

- **Status:** Accepted
- **Date:** {{DATE}}
- **Scope:** Project-wide
- **Reversibility Cost:** Low — the records can be abandoned; the decisions they describe cannot be un-made
- **Decision Drivers:** lost rationale, repeated debates, contradictory later decisions
- **Supersedes / Superseded by:** —

## Context

Architecturally significant decisions were made in reviews, chats and pull-request
threads. Their rationale was not kept in one place. A later contributor could not tell
whether an alternative had already been considered, whether a constraint was still
current, or whether a new choice contradicted an earlier one.

## Decision

We will record every architecturally significant, costly-to-reverse decision as an
Architecture Decision Record under `docs/adr/`, one file per decision, following the
standard summarised in `README.md`: a metadata block, Context, Decision, Consequences,
and a Related Decisions section that vets the record against the decisions already in
force. A decision changes by a new record that supersedes or amends the old one; an
accepted record is never edited or deleted. `check_adr.sh` enforces the structure.

## Consequences

Easier: a contributor finds why the system is shaped as it is, and which alternatives
were rejected; a new decision is checked against the existing set before it lands;
`INDEX.md` gives the whole decision landscape in one table.

Harder: each qualifying decision costs a short document and a review; authors must sweep
the prior records before acceptance. Routine choices stay out of `docs/adr/`, so the
corpus stays small.

## Related Decisions

Vetted against prior ADRs on {{DATE}}:

No prior decisions to vet against.
