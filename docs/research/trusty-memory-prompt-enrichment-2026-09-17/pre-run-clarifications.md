# Pre-run audit corrections and design clarifications

These changes precede all tuning and heldout retrieval runs. No result-driven labels or weights changed.

The fixture audit identified inventory assertions whose observation date was August 15 but whose effective date defaulted to May 1. The current native graph page sorts by effective `valid_from`, so that fixture did not establish the claimed crowdout by newer assertions. All 1,320 inventory assertions now use August 15 for both dates. Source prose, query wording, and gold labels are unchanged. The input manifest is refreshed after this pre-run audit.

The improved graph projection models literal assertions as properties of an entity. A hop traverses an explicit entity-to-entity relationship; properties of reached entities do not consume an additional hop, but every inspected property and relationship counts against the examined-fact budget and every emitted fact counts against the output cap. Native graph controls retain their native triple-edge hop semantics. This difference is intentional and must remain visible in reporting.

Shared prompt packing renders complete claim text through the public formatter and retains revision/span provenance in a machine-readable sidecar. The sidecar is not part of the prompt-token budget; all treatments use the same convention. Evidence checks validate the sidecar against actual included claim text. This measures enrichment content, not an LLM's ability to produce citations from a separate sidecar.

Native standing/current-policy controls retain the public formatter's actual representation, which can omit a triple's subject and render only its object. Their evidence check requires that exact public-rendered bullet plus the originating triple and source-span sidecar, rather than incorrectly requiring a full source sentence the native formatter never emitted. Main treatment claims still require complete verbatim source sentences. Native controls are therefore labelled separately; their scores do not establish equal attribution quality or answer equivalence.

The canonical `gold.json` is retained for audit. Identical rows are also partitioned into `gold-tune.json` and `gold-heldout.json`; the runner parses only tuning gold until it has written selection, then opens heldout gold. Hash verification may read bytes of both without interpreting labels. Standing-only queries are excluded from task F1/coverage and task-empty denominators and reported separately.
