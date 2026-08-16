Fixed
- The stale-aside sweep no longer silently discards additional stranded
  copies: when the destination is missing and MORE than one dead-pid aside
  exists for the same binary, the first (sorted) aside is restored and every
  later one is deleted with an explicit warning naming the file. Previously
  the later copies — whose contents may differ from the restored one — were
  destroyed with no distinct trace, purely because the first restore made the
  destination exist (#5777, trusty-review round on PR #5778).
