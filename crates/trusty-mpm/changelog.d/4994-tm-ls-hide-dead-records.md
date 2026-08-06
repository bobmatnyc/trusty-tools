Fixed

- `tm ls` no longer shows dead session records in its default view (closes [#4994](https://github.com/bobmatnyc/trusty-tools/issues/4994))
  - a record the daemon flags `unresumable` — no tmux pane, no workspace directory left on disk — was rendered `[dead]` but still listed, because `is_live_session_state` consulted lifecycle state alone
  - the records are untouched: `tm ls --all` still lists every one, and the remedy is unchanged (`tm sessions delete <id> --force`)
  - the default-view filter now runs AFTER the listing-time auto-prune, so the sweep still sees the records it reaps
  - the "N more dead records pending confirmation" line now says where those rows went
