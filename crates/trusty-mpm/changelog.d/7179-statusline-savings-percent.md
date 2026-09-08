Changed

- The `💸` statusline segment now shows the whole-number percent of tokens the session avoided sending (`💸34%`), replacing the prior dollar/token-count figure. The percent is folded from the savings ledger's own `tokens_saved`/`tokens_before` sums, never from the session's live context-window fill. `0%` is never rendered; the segment is omitted when nothing was saved.
