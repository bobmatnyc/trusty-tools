## Verdict: WARN

Reviewed the experimental JSON Rust harness against `docs/research/trusty-memory-deterministic-2026-09-17/{README,interface,evaluation}.md`. No production daemon adapter is in scope. Repository files were not modified.

## Findings

Paths below are relative to `/Users/masa/trusty-search-experiment/worktree/crates/trusty-memory/examples/support/memory_deterministic/`.

| Severity | File | Line | Issue | Fix | Disposition |
|----------|------|------|-------|-----|-------------|
| HIGH | `retrieval.rs` | 136–148 | `seeds.extend(...)` finds exact aliases but never inserts those identities into `candidates`. A source without outgoing links disappears when its alias is excluded by the context token cap, even though the KG treatment promises explicit alias retrieval. | Insert eligible exact-alias matches as bounded KG candidates, with zero lexical rank/BM25 score when absent from lexical results. Count them toward the extra-candidate cap; retain one-hop traversal. Test ambiguous aliases excluded from context. | Fix here |
| HIGH | `maintain.rs` | 104–110 | `fresh` checks only matching derived rows and `documents.contains_key(&r.doc_id)`. Extra legacy snapshot children for the same source are ignored. `validate.rs:267–270` similarly checks expected documents exist without rejecting extra children. A stale extra child can retrieve a source with `index_fresh:true`; `retrieval.rs:203–204` substitutes the current whole body when that child has no derived row. | Require a complete, consistent per-source snapshot/derived child set before declaring the source fresh, or explicitly track freshness per selected child. Reconciliation must remove obsolete children, and stale-child fallback must report `index_fresh:false`. Add a mixed legacy/derived regression. | Fix here |
| MEDIUM | `validate.rs` | 98–106 | `context_tokens <= 512`, `(1..=4096).contains(&chunk_tokens)`, freshness `0..=0.3`, and KG `0..=0.5` disagree with the interface's `[1,256]`, `[32,1024]`, `[0,0.20]`, and `[0,0.30]`. Invalid experiment configurations succeed, including zero context tokens. | Enforce the documented bounds and test each accepted boundary plus one value outside it. | Fix here |

## Required Changes

1. Make exact-alias matches candidates independently of lexical enrichment and test the context-cap case.
2. Prevent unproven legacy children from claiming fresh coverage; verify reconciliation removes them.
3. Align policy validation with the frozen interface.

## Verification Results

The supplied example-test log contains:

```text
test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
```

Independent probes invoked `/Users/masa/trusty-search-experiment/target/debug/examples/memory_deterministic_eval` through Python `subprocess.run`, passing JSON on stdin. Raw observations:

```text
exact_alias_beyond_context_cap 0 [{"hits": [], "id": "q"}]
out_of_contract_policy 0 True
extra_legacy_child_claimed_fresh 0: excerpt="body", index_fresh=true, bm25_score=0.6931471824645996
```

Probe inputs, sufficient to reproduce:

- Common source: identity `(s,a)`, revision 1, room `aaa`, body `body`, alias `zulu`, created `2026-01-01T00:00:00Z`; query current at `2026-09-01T00:00:00Z` in scope `s`.
- Alias probe: treatment `kg`, query `zulu`, policy `{context_tokens:1,chunk_tokens:32,freshness_weight:0.05,kg_weight:0.15}`; sufficient publication budgets. The cap includes `aaa` and excludes `zulu`; exact alias still exists in authoritative input. Response has no hits.
- Policy probe: same request, policy `{context_tokens:0,chunk_tokens:1,freshness_weight:0.3,kg_weight:0.5}`. Response succeeds with exit 0.
- Legacy probe: first index that source with `repaired_raw`; append snapshot `{doc_id:'["s","a",99]',text:'obsolete'}` to returned state, leaving derived child 0 unchanged. Resume with zero maintenance budgets and query `obsolete`. The extra child matches; the returned current excerpt is `body` with `index_fresh:true`.

Status: NEEDS ATTENTION. The full crate gate remains owned by the parent; no duplicate suite was started. This review does not claim production compatibility or production runtime verification. Parent continues with these findings and the separate full-crate gate.

## Final recheck disposition: APPROVE

All three findings are resolved in the final source. No remaining issues found at >80% confidence within this bounded recheck. APPROVED for next pipeline stage.

- Exact alias seeds enter the bounded candidate set independently of context enrichment; new coverage includes ambiguous aliases with no links.
- Fresh coverage now checks the complete snapshot child count, and validation rejects extra children beside a claimed current generation. Legacy-only coverage still repairs successfully.
- Policy limits match the interface, with accepted-boundary and rejected-boundary tests.

Independently verified binary SHA-256: `32cacc0a86aad18d69b10c21dc66090f803f19592866904093c57566863a7c82`.

The original three probes and one legacy-repair continuation produced:

```text
alias probe: exit=0 id=a origins=[kg] bm25_score=0 PASS
policy probe: exit=2 invalid_request PASS
mixed child probe: exit=2 invalid_state no replacement state PASS
legacy repair probe: exit=0 snapshot_rows=1 obsolete_query_hits=0 PASS
```

The latest example-test log contains:

```text
test result: ok. 13 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
```

No builds, benchmark runs, Git mutations, or repository edits were performed in this recheck. Full-crate results remain parent-owned evidence. Parent continues with final benchmark reporting.

## Policy v2 amendment recheck: APPROVE

Bounded review of `amendment-01.md`, the revised `derive.rs`, its regression test, and v2 policy/interface declarations found no issues at >80% confidence. APPROVED for next pipeline stage.

The revised body limit counts shared-tokenizer expansions per whitespace run, retains repeated runs, and caps source slices at 4096 UTF-8 bytes. The split path preserves contiguous source ranges; unsplit control treatments retain whole bodies. The new regression covers repeated vocabulary, compound identifiers, oversized UTF-8 runs, whitespace, and exact reconstruction.

Independently verified final binary SHA-256: `679b2842c2891afa9707a58fb68f4fd01ec7f9bdf91c267b68d4f42f09a0cd19`.

Five bounded subprocess probes produced:

```text
repeated128: PASS children=6 maximum_source_bytes=896 exact_coverage=true policy=v2
repeated256: PASS children=3 maximum_source_bytes=1792 exact_coverage=true policy=v2
utf8: PASS children=3 maximum_source_bytes=4096 exact_coverage=true policy=v2
compound: PASS children=8 maximum_source_bytes=275 exact_coverage=true policy=v2
whitespace: PASS children=2 maximum_source_bytes=4096 exact_coverage=true policy=v2
```

No builds, benchmark runs, or repository edits were performed. Run-02 and comparative conclusions remain parent-owned; the amendment correctly discloses that the held-out set was already observed in run-01.
