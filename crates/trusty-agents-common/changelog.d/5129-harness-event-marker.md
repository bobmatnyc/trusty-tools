Fixed

- `events::EVENT_LINE_PREFIX` is now `"__OMPM_EVENT__ "` — the prefix trusty-code
  and trusty-agents have always written to stderr. It read `"__HARNESS_EVENT__ "`,
  and the bundled harness-understanding assets repeated that value, so the
  trusty-mpm session manager was told to watch for a marker no harness emits
  ([#5129](https://github.com/bobmatnyc/trusty-tools/issues/5129)). The constant
  is now the single declaration both harness crates re-export, and
  `harness_doc_names_the_relay_prefix` pins the assets to it instead of to a copy
  of its text.
