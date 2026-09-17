# Deterministic memory query plans

[Results](report.md): structured requests reached 90% positive coverage and 100% negative abstention on 32 fresh heldout queries, compared with 60%/33.3% for the prior policy. Defect fixes with flat requests reached 65%/100%. Embeddings are excluded.

Read the [interpretation](interpretation.md) for attribution and generalization limits, [protocol](protocol.md) and [interface](interface.md) for frozen behavior, [fixture notes](fixture-notes.md) for independent labels, and [verification](verification.md) for checks. The [experiment](../../../experiments/trusty-memory-query-plan/README.md) contains source, fixtures, manifests and compressed packets. Production integration remains open in [#8246](https://github.com/bobmatnyc/trusty-tools/issues/8246).
