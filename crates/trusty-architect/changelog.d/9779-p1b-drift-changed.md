Changed
- Context bars follow `ctx_bar.py`: PM `ctx` alerts at 31/35% (were 22/25%); the Architect's `self_ctx` threshold is its bar from `thresholds.architect_bar_ctx` in `il-schedule.json`, 35% by default (#9779).
- The builder band stops on swap activity of 256 pages or more per sample, not on swap left in use; the band detail reads `swap activity N pages` (#9779).
- No 5-hour `plan-limit` alert; `tm-architect-questions` raises no `idle` alert; the ILIO trial sessions are not watched; `parse_pane` reports `rc_active` (#9779).
