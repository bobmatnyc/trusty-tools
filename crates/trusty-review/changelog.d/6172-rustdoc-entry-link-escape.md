Fixed

- A doc comment on `parse_entry_node` in `trace_client.rs` tried to escape backticks inside a markdown code span with `\``, but backslash does not escape inside a CommonMark code span — the span closed early and `[ENTRY]` fell outside it, where rustdoc read it as an unresolved intra-doc link. The span now uses a longer backtick-run delimiter (` `` ` instead of `` ` ``) so the inner backticks need no escaping. Caught by the pre-publish rustdoc broken-link gate ahead of the 0.24.0 release.
