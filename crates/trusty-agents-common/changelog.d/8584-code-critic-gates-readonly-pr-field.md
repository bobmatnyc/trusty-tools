Changed
- `code-critic` agent prompt: gates are read-only (cite the engineer's reported gate output, never re-run one), and the brief carries `PR: <n>|none`; with `none` the critic posts nothing and makes no PR lookup (Refs #8584).
