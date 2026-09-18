Changed
- PM prohibition P10 gains one narrow exception: a read-only `tmux capture-pane`
  of the PM's own session pane, filtered at source to agent status lines, so the
  PM can observe its dispatched agents' elapsed time and token burn. Every other
  tmux verb, every other pane, and every other non-git Bash command stay
  forbidden. The PM Allowlist carries the matching entry.
- Fixed the documented filter: `-S -80` now reaches scrollback, where agent
  status rows actually sit, and the pattern anchors on the row's own leading
  `  ◯ ` shape so it stops self-matching the echoed command or PM prose that
  quotes the filter. `tm-delegation-patterns`'s "PM Allowlist, in Full" table
  now carries the same carve-out as the two instruction sections.
