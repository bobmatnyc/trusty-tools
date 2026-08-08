Fixed

- Updated doc comments that still named `open-mpm` as the caller of the
  read-only CTO DB tool surface (`tool_list_response`, `handle_tool_call`, and
  the crate-level overview) to say `trusty-agents` (renamed in #831). Doc
  comments only — the OMPM-RPC/1 wire contract and every query are unchanged.
