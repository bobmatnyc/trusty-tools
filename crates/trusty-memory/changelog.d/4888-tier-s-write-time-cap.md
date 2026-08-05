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
    auto-discovery loop, the **chat assistant's `kg_assert` tool** (`POST /api/v1/chat`),
    `POST /api/v1/palaces/{id}/kg`, `POST /api/v1/kg/aliases`, and the offline
    `kuzu-migrate` relation import. Two of these were live bypasses rather than
    hypothetical ones: the chat tool takes `predicate`/`object` straight from the
    model on a surface users hit every turn, and the HTTP KG endpoint is where
    `trusty-mpm`'s provisioner seeds its identity fact
  - the cap cannot be raced past: counting active facts and then writing is two
    steps, and nothing else serialized them — the KG's single-writer actor orders
    writes only within one palace while the count spans all of them. A new
    admission mutex is held from the count through the write. Measured before the
    fix: 16 concurrent writers contending for 1 free slot were all admitted,
    landing the surface at 35
  - the offline `kuzu-migrate` import refuses hot predicates outright rather than
    counting free slots. A bulk legacy import is not a deliberate act of authoring
    a standing rule, which ADR-0028 D8 point 3 requires, so legacy relation data
    never reaches Tier S no matter how much room is left. Refusals join the
    existing warn-and-skip path and name `kg_assert` as the way to author the fact
    deliberately, where the real gate applies. This removes the cap arithmetic
    from that path entirely, and with it any way for the offline gate to be off by
    one or to fail open
  - `discover_aliases` is the one bulk writer, and it stops at the cap instead of
    aborting: aliases that fit are written, the rest come back in a new `rejected`
    array with a single `rejected_reason`, alongside a `complete` flag so a caller
    reading only `new`/`already_known` cannot mistake a partial batch for a whole
    one. A workspace with more crates than Tier S has slots is ordinary — this one
    has — so aborting would both make the tool unusable there and strand the
    aliases written before the refusal
  - the cap counts ACTIVE facts only: retracting a fact frees its slot immediately,
    since retraction closes the interval rather than deleting the row
  - re-asserting an already-active `(subject, predicate)` in the same palace is a
    replacement, not an addition, and stays admitted at the cap — otherwise an author
    who filled the surface could never correct an existing rule
  - cold (non-hot) predicates are untouched: neither limit applies to ordinary
    knowledge-graph writes, which never reach the injected surface
