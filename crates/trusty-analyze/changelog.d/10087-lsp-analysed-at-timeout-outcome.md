Fixed

- `analyze.lsp_analysed_at` no longer ends its bus event with `outcome: ok` when the barrier timed out. The terminal event now carries `state` (`ready` or `timed_out`), plus `version_seen` on a timeout, and a timeout reports `outcome: timed_out`; the ready case is unchanged and the RPC answer is unchanged (Refs #10087).
