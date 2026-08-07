Fixed

- `session_context_catchup` returns the paused sessions that actually postdate the watermark ([#5072](https://github.com/bobmatnyc/trusty-tools/issues/5072))
  - the tool returned exactly one session — a hand-written snapshot with every field empty or null — while `resolved_snapshot` pointed at a different, well-formed file. A resume driven off `sessions[]` restored the wrong snapshot; one driven off `resolved_snapshot` restored a file that had never been parsed
  - the response gains `undatable_sessions_dropped`. An empty `sessions` array now only means "nothing paused since your last catch-up" when that count is 0; non-zero means sessions exist but could not be dated and were withheld, and the caller should re-call with `full`
  - `resolved_snapshot` and `sessions[]` still disagree under a recent watermark, by design — they answer "what should I resume from" and "what paused since your last catch-up". What is fixed is the inversion, not the disagreement
  - `recent_commits` / `recent_memory` coming back empty is unchanged and correct — a non-`full` call reports only what is newer than the stored watermark
