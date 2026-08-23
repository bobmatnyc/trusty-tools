Fixed

- `FilterConfig` with a non-default `reject_patterns` set no longer recompiles
  and `Box::leak`s its regex set on every `apply` call (#6199). A custom set is
  now memoised process-wide keyed by its pattern strings, so each distinct set
  compiles exactly once and callers get a cheap `Regex` clone — the default set
  keeps its own shared cache. `Box::leak` is gone from the gate entirely. The
  credential detector moved to a `filter::secret` submodule so the gate module
  stays under the SLOC cap after the fix; behaviour is unchanged.
