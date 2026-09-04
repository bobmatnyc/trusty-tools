Fixed

- The analyze lane's own outcome is now a recorded fact in Gaps & Caveats, not
  something a reader has to infer from the per-repository reason lines. A run
  where every repository's fetch failed leads its gap list with
  `trusty-analyze lane DID NOT RUN — 0 of N application(s) assessed, N failed`,
  and a partly degraded run states `M of N application(s) assessed`. Before this,
  a 59-repository bundle whose analyze lane never ran carried the same shape of
  line as one where the lane worked for 58 of 59, so every CAST health factor
  read as absent rather than unassessed and downstream readers concluded static
  analysis had run and found nothing. Per-repository fail-open in
  `analyze_adapter.rs` is unchanged: nothing aborts, and a lane that populated
  everything it attempted still adds no line (#6811).
