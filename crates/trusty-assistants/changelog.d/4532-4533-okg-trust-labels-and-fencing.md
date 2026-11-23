Added

- OKG content now carries a **trust label**, and untrusted content is **fenced
  before it reaches a model turn** (closes
  [#4532](https://github.com/bobmatnyc/trusty-tools/issues/4532) and
  [#4533](https://github.com/bobmatnyc/trusty-tools/issues/4533); epic
  [#4531](https://github.com/bobmatnyc/trusty-tools/issues/4531), DOC-63 §6.3).
  This repo has three defences against untrusted ingested content and had built
  two — capability reduction (test-pinned by `bundled_personas_pin_git_reach`)
  and prompt-level fencing (`UNTRUSTED_PREAMBLE`, memory drawers only). The
  third did not exist: nothing labelled the content itself, so a chunk
  retrieved through `vector_search` arrived at the model with no provenance and
  no fence. An assistant's memory drawers were fenced; its knowledge store was
  not.
  - **The label.** `trusty_kb::okg::trust::TrustLabel` is stamped into every
    ingested entity's frontmatter as `trust: untrusted-external` by the ENGINE,
    derived from the operator-written `SourceSpec` alone. A connector cannot
    mark its own output trusted: the label is written into the frontmatter
    envelope before connector-supplied fields are merged, and that merge already
    skips keys the envelope claimed. The single carve-out is a local directory
    the operator explicitly designated `user_authored`; the flag is ignored, not
    honoured, on any remote locator, so a Gmail SENT-only window cannot launder
    itself into the trusted set (DOC-63 §6.4 `S-4.8`).
  - **The fence is the SAME fence.** The envelope delimiters, the per-line
    neutralizer and the untrusted preamble were LIFTED out of
    `ctrl::pm_task::dispatch::persona_memory` into `crate::untrusted` and both
    paths now call them. There is no second implementation — a divergence
    between two fences is a security bug, not a style issue.
    `memory_preamble_is_byte_identical_to_the_pre_lift_constant` pins that the
    lift changed zero prompt bytes on the drawer path, and
    `both_fences_carry_the_same_rules` fails if the two ever drift.
  - **Fail closed.** A hit whose label cannot be established — no `trust` key
    (the entire pre-#4532 corpus), an unreadable or vanished file, unparseable
    frontmatter, or a value this build does not recognise — is fenced as
    untrusted. Labels arrive incrementally over a corpus that already exists, so
    the unmigrated majority takes this path.
  - **Scope.** Only the agent's own bound OKG store is fenced. An attached
    tier-2 index (#3232/#4009), an explicitly-named foreign corpus, the embedded
    local code index and the grep fallback all keep their prior output byte for
    byte. `vector_search`'s schema description states the new result shape only
    for agents that actually have a bound store, so an unbound agent's tool
    definition is unchanged.
  - **Not a guarantee.** Fencing is a mitigation; no delimiter reliably survives
    an adversarial instruction (DOC-63 §6.5). The load-bearing control remains
    capability reduction, untouched here and still pinned by
    `bundled_personas_pin_git_reach`.

Known limitation

- DOC-63 `S-4.4` prescribes carrying the label "into the trusty-search chunk
  payload by the index feed". **That mechanism does not exist**:
  `POST /indexes/{id}/index-file` accepts exactly `{path, content}`, a returned
  `CodeChunk` has no metadata map, and the markdown chunker keeps YAML
  frontmatter in the FIRST chunk only — so a file-level label would reach one
  chunk out of N, which is the "label stops at the file" failure `S-4.4` exists
  to forbid. The label is therefore resolved per hit from the hit's own absolute
  path at the point of use, which reaches EVERY chunk rather than the one that
  happened to contain the frontmatter. Giving trusty-search a real per-chunk
  attribute channel would let the resolution move into the index; it is not
  needed for correctness here and is not owned by this ticket.
