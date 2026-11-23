Fixed

- **A failed vector write no longer leaves a memory that `get` returns and
  `search` can never find.** `insert` committed its redb transaction before
  touching the usearch index under a separate lock, so any failure between the
  two — or a crash — recorded the payload and label with no vector behind them.
  The segment mutex is now taken before `begin_write` and redb commits last, so
  a failed vector write aborts the transaction and leaves redb untouched; the
  only residue is an orphan vector whose label has no redb row, which `search`
  already skips
- **`move_segment` can no longer duplicate a record across two segments.** It
  inserted into the destination and deleted from the source in two independent
  transactions, biased toward duplication when the second step failed. Both
  halves now share one redb transaction, with the destination vector flushed
  before the commit and the source vector dropped after it, so a failure leaves
  the record whole in exactly one segment. A crash between the commit and the
  source flush still strands an unreachable vector in the source index, which
  the next write to that label reclaims
- **Search scores are clamped to `[0.0, 1.0]`.** `1.0 - distance` went to
  `-1.0` for a vector antipodal to the query, because cosine distance reaches
  `2.0`

Found by the 2026-08-19 trusty-tools self-audit, whose summary named
crash-consistency weaknesses in the semantic-memory store as a top concern.
