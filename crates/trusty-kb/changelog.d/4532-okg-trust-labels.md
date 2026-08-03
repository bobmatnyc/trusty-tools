Added

- Every OKG-ingested entity now carries a **trust label** in its frontmatter
  (closes [#4532](https://github.com/bobmatnyc/trusty-tools/issues/4532); epic
  [#4531](https://github.com/bobmatnyc/trusty-tools/issues/4531), DOC-63 §6.3
  `S-4.3`). Per-entity provenance already existed — `okg::ingest` stamps
  `source_id`, `source_kind` and `source_item_id` — but it is DESCRIPTIVE
  metadata that nothing downstream read as a trust signal, and nothing could,
  because "which of these source kinds is trusted?" was a judgement no caller
  was equipped to make. `okg::trust::TrustLabel` makes that judgement once, in
  the engine, and records the answer as `trust: untrusted-external`.
  - **A connector cannot mark its own output trusted.** `TrustLabel::for_source`
    takes only the operator-written `SourceSpec`; the fetched `SourceItem` is
    deliberately not a parameter, so no future connector can reach the decision
    even by accident. `write_item` stamps the label into the frontmatter
    envelope BEFORE merging connector-supplied fields, and that merge already
    skips keys the envelope claimed — so a fetcher emitting
    `trust: user-authored` is shadowed out rather than honoured.
  - **Untrusted is the default, and the only default.** The single carve-out is
    a `doc_store` locator whose row carries the new
    `SourceSpec::user_authored` designation. On any remote locator that flag is
    ignored, not honoured: no remote corpus has an enforceable author
    constraint, so a Gmail SENT-only window cannot launder itself into the
    trusted set (DOC-63 §6.4 `S-4.8`/`S-4.9`).
  - **Reading it back fails closed.** `TrustLabel::of_entity_file` resolves to
    `untrusted-external` on every failure mode — missing file, unreadable file,
    no frontmatter, no `trust` key, unparseable YAML, or a value this build does
    not recognise. Labels arrive incrementally over a corpus that already
    exists, so the unmigrated majority must be safe (`S-4.6`).
  - A tombstone preserves the label: `tombstone_item` merges rather than
    overwrites, so a vanished item's recorded provenance survives being flagged.
  - `SourceSpec::user_authored` is `#[serde(default)]`, so every registry row
    written before this field existed loads unchanged — and loads UNTRUSTED,
    which is the fail-closed direction for a defaulted boolean.

The retrieval-side fence that consumes this label lives in `trusty-agents`
(`crate::untrusted`, `tools::memory::okg_fence`) — see that crate's entry for
[#4533](https://github.com/bobmatnyc/trusty-tools/issues/4533).
