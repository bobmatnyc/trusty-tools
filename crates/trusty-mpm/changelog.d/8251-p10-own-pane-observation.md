Changed
- PM prohibition P10 gains one narrow exception: a read-only `tmux capture-pane`
  of the PM's own session pane, filtered at source to agent status lines, so the
  PM can observe its dispatched agents' elapsed time and token burn. Every other
  tmux verb, every other pane, and every other non-git Bash command stay
  forbidden. The PM Allowlist carries the matching entry.
