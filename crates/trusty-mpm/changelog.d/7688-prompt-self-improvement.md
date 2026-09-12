Added
- `prompt-self-improvement`: a `prompt_self_improvement` boolean in `[pm]` of `~/.trusty-mpm/config.toml` and at the top level of a project's `.trusty-mpm.toml`, default off. When on, the composed PM prompt and every deployed agent ask for a `## Prompt feedback` addendum of at most 5 lines — what was unclear, and what was unnecessary.
- `tm hook --prompt-feedback`, registered on `Stop` and `SubagentStop` only where the flag is on, captures that section into `~/.trusty-mpm/prompt-feedback.jsonl`. It fails open: an unreadable transcript or ledger logs a warning and exits 0.
- `tm prompt-feedback` reads the ledger back, newest first, with `--session`, `--agent`, `--limit`, and `--summary`.
