Added

- Tier S write-time admission control — a hard cap of 20 standing facts and an 80-character form constraint (ADR-0028 D2/D8, closes [#4888](https://github.com/bobmatnyc/trusty-tools/issues/4888))
  - the four hot predicates (`is_alias_for`, `has_convention`, `is_fact`,
    `is_shorthand_for`) feed the always-injected prompt surface, which is paid for on
    every turn of every session; the 21st fact is now REJECTED at write time rather
    than silently dropped or truncated at read time
  - the rejection is actionable: it names all 20 facts currently occupying the surface
    with their `subject`/`predicate`, and names `remove_prompt_fact` as the tool that
    retires one, so the caller can choose what to trade away
  - an object longer than 80 characters is rejected with its actual length and the
    limit; a rule that does not fit is a document and belongs in `CLAUDE.md` with a
    pointer
  - both rejections are fail-closed — the write does not reach storage
  - enforced on every path that can create a hot triple, not just the two obvious
    ones: the `kg_assert` and `add_alias` MCP tools, the `discover_aliases`
    auto-discovery loop, `POST /api/v1/palaces/{id}/kg`, and `POST /api/v1/kg/aliases`.
    The HTTP KG endpoint is the path `trusty-mpm`'s provisioner uses to seed its
    identity fact, so it was a live bypass rather than a hypothetical one
  - `discover_aliases` is the one bulk writer, and it stops at the cap instead of
    aborting: aliases that fit are written, the rest come back in a new `rejected`
    array with a single `rejected_reason`. A workspace with more crates than Tier S
    has slots is ordinary — this one has — so aborting would both make the tool
    unusable there and strand the aliases written before the refusal
  - the cap counts ACTIVE facts only: retracting a fact frees its slot immediately,
    since retraction closes the interval rather than deleting the row
  - re-asserting an already-active `(subject, predicate)` in the same palace is a
    replacement, not an addition, and stays admitted at the cap — otherwise an author
    who filled the surface could never correct an existing rule
  - cold (non-hot) predicates are untouched: neither limit applies to ordinary
    knowledge-graph writes, which never reach the injected surface
