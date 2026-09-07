Fixed

- `compress::classify_tool` no longer lets a file's own path choose the filter
  that damages it. The tool name it receives is the wrapped command, so
  `cat crates/x/src/tests.rs` matched the `test` substring branch and
  `filter_test_runner` returned an empty string — the whole file, silently
  gone — while `cat …/mod.rs` reached `filter_file_read`, which strips every
  `//` line and so removed the doc comments being read. A read verb (`cat`,
  `head`, `sed`, `bat`, `tail`, `nl`, `less`, `more`) applied to a path with a
  source or prose extension now classifies as `None` and passes through
  byte-for-byte. Gate captures keep their compression: `.txt`, `.log` and
  `.out` are deliberately not source extensions (#6986).
- `compress::compress_tool_output` returns the original whenever a filter
  consumed every byte of a non-empty input, so a compression path can no
  longer emit nothing at all (#6986).
