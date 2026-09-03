Fixed
- `catchup::session_log::append_entry` writes a log line and its trailing
  newline in one `write` instead of two. `writeln!` on a `File` formats
  piecewise, so each pause cost two `write(2)` calls; `O_APPEND` makes an
  individual write atomic but not a pair of them, so two concurrent pauses could
  interleave into `<lineA><lineB>\n\n` — one unparseable line plus one blank,
  which `read_log` skips as malformed and the record is gone
  ([#6732](https://github.com/bobmatnyc/trusty-tools/issues/6732)). A run of
  1024 concurrent appends corrupted 439 lines before the change and none after.
