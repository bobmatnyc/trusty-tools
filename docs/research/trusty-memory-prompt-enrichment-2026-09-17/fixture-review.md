# Pre-run fixture and protocol audit

Read-only audit, 2026-09-17. Files: `experiments/trusty-memory-prompt-enrichment/{sources,queries,gold}.json`, manifest, and `docs/research/trusty-memory-prompt-enrichment-2026-09-17/{interface,protocol}.md`. No retrieval/embedding/ranking run. Heldout labels were read only for independent audit. No implementation received gold from this audit.

## Verdict: WARN pending one pre-run correction

**The newest-200 crowdout premise is currently false by fixture timestamps.** All 1,320 inventory facts have `observed_at=2026-08-15` but `valid_from=2026-05-01`, the same validity start as most useful facts. Production `KgStoreRedb::list_active` sorts by `valid_from`, not observation time (`crates/trusty-common/src/memory_core/store/kg_redb/read_ops.rs:146`). Thus the protocol's "220 recent structural inventory assertions" do not establish the intended newest-first masking mechanism. Parent acknowledged correction: set their validity starts to August 15, document the pre-run amendment, and regenerate manifest. Claims, labels, and queries remain unchanged. No outcome exposure precedes this correction.

## Independent checks observed

Executed a read-only Python audit independent of experiment implementation:

```text
AUDIT_ERRORS []
COUNTS 1422 1434 12 102 required_groups 96 empty 24
tune queries 51 task_answerable 36 required_groups 48 task_empty 12 standing_only 3
heldout queries 51 task_answerable 36 required_groups 48 task_empty 12 standing_only 3
```

Checks cover all claims' UTF-8 byte spans, duplicate evidence identities, every gold reference, required inclusion in acceptable, forbidden/acceptable disjointness, complete event replacement, selected source revision, deletion, exact scope, knowledge cutoff, source expiry, and fact interval eligibility for every acceptable claim. All five manifest hashes matched at audit time, before the acknowledged amendment.

The two-hop requirements contain project→team, team→contact, and contact calling hours. Other contact/incident-route facts are acceptable supporting context. The one-hop requirement contains project→team plus team channel. This is coherent with the parent clarification: literal properties attach to the reached entity and do not consume another relation hop; actual native graph controls retain their native hop semantics.

## Denominators and interpretive limits

- Per split, 36 task-answerable queries carry 48 required groups, 12 queries are task-empty negatives, and 3 are standing-only. Standing-only rows have no task requirements and `expected_empty=false`; do not count them as either task-positive recall successes or task-negative abstentions. Exclude them from task macro quality. The standing stratum is reported independently.
- Shared standing content is acceptable everywhere and excluded from task precision/recall/F1. Prelude removal must also apply to task useful-facts-per-token reporting; retain the full-packet token metric separately.
- Every graph relation has supporting source prose. The graph receives explicit entity and predicate structure, but no relevance labels. This fairly compares curated structured indexing against identical underlying information; it does not test whether extraction can create that structure from prose.
- Paraphrase prompts still contain the subject's literal name. They test semantic ranking among that entity's facts, not entity-free semantic discovery. Shared scenario templates across tune/heldout remain a stated limitation despite changed wording.
- Current/historical owner labels agree with validity boundaries. Future-known key location is correctly excluded by knowledge cutoff. Updated/deleted scenarios apply complete replacement events, and gold uses only revision 2 where required. Events retain old observation timestamps; this tests index revision repair, not a newly observed later fact or bi-temporal event replay.
- The ambiguity task requests both alias meanings. Suppressing ambiguous resolution must not suppress the ambiguity evidence itself. A lexical alias prefix must not turn a longer ambiguous alias into the shorter unambiguous alias's target.

## Focused-test requirements beyond relevance fixture

Observed main-fixture counts:

```text
future_effective 0
maximum substantive subject outdegree 4
tags 1320
```

The relevance fixture has no future-effective assertion and no substantive graph cycle. Its large hub consists of structural `tags` edges, excluded by the new projection. Consequently it cannot itself prove scanned-edge caps on substantive hubs or cycle termination. The protocol already requests separate focused/scaling fixtures; these must include substantive high degree, cycle/duplicate paths, future-validity boundaries, conflicting slot ties, and insertion-order perturbations. Report those as mechanism checks rather than additional quality samples.

No other concrete wrong gold label was found. Parent continues the timestamp correction, freeze verification, and implementation. Research audit artifact: `/tmp/graph-fixture-review.md`.


## Resolution: PASS, corrected pre-run freeze

Rechecked after the correction, before ranking runs. All 1,320 inventory assertions now have `valid_from=2026-08-15T00:00:00Z`. All eight manifest entries match their files. Canonical gold retains its original SHA256; split files contain exactly the corresponding 51 canonical rows in each split, preserving order.

The hashed `pre-run-clarifications.md` explicitly records entity-relation hop semantics, native graph edge semantics, complete-claim versus native-bullet evidence representation, sidecar token accounting, gold parsing isolation, and standing-only task-denominator exclusions. The identified fixture defect is resolved. Focused-test and synthetic-workload limitations above still apply.

Observed verification:

```text
split tune identical rows 51
split heldout identical rows 51
PASS: 1320 corrected timestamps; 8 manifest files match; canonical gold unchanged
```

Frozen SHA256 entries:

```text
e61e69f1ecb265fd5034f6901222de6d8cb1a2840c4517a7e20b40e2b444fabd  experiments/trusty-memory-prompt-enrichment/sources.json
9440e61f87348716d474ddd8456d0297e510f2f97ef8efd4dbd630b5c4797099  experiments/trusty-memory-prompt-enrichment/queries.json
bb3babac5e65e3c9642ed8d16d0c4f5794f17398ec15c0fa271714e8c1d5f57b  experiments/trusty-memory-prompt-enrichment/gold.json
439abbf3b5b0adc5c0e2b42bce2362e311b2369c66c83a139d03c7fef9a79c20  experiments/trusty-memory-prompt-enrichment/gold-tune.json
ecd4e9d0b33054be05adca780d18809a345dffaec8a60d65ef61b36070f23ecd  experiments/trusty-memory-prompt-enrichment/gold-heldout.json
9b0697296abfefded24a911a304c4e9c75786a66f20d2e26231375dab3330d44  docs/research/trusty-memory-prompt-enrichment-2026-09-17/protocol.md
3bc988b9b549393ae8ecd0077c44df003ee6795d643b5b9cc902fb787b6ab70b  docs/research/trusty-memory-prompt-enrichment-2026-09-17/interface.md
af17f5ee7fea9781689c72774009a293393f27c3eba434e96561b36a0d70c9ed  docs/research/trusty-memory-prompt-enrichment-2026-09-17/pre-run-clarifications.md
```
