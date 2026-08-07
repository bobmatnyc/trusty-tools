Fixed

- `session_context_catchup` returns the paused sessions that actually postdate the watermark ([#5072](https://github.com/bobmatnyc/trusty-tools/issues/5072))
  - the tool returned exactly one session — a hand-written snapshot with every field empty or null — while `resolved_snapshot` pointed at a different, well-formed file. A resume driven off `sessions[]` restored the wrong snapshot; one driven off `resolved_snapshot` restored a file that had never been parsed
  - the cause was in the shared `trusty_common::catchup` engine this crate re-exports, fixed there; see that crate's fragment for the mechanism. `recent_commits` / `recent_memory` coming back empty is unchanged and correct — a non-`full` call reports only what is newer than the stored watermark
