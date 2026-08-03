Documentation

- `ToolResult::is_error` / `is_fatal` no longer point their `Test:` doc
  comments at a test in the `cto-assistant` crate, which #3732 deletes. The
  assertions those pointers named are restored as unit tests beside the
  predicates they actually cover
  (`tool_result_is_error_distinguishes_variants`,
  `tool_result_is_fatal_only_for_non_recoverable`), so the crate's
  grandfathered row in `.test-pointer-allowlist.tsv` could be retired instead
  of left permanently unresolvable. No behaviour change.
