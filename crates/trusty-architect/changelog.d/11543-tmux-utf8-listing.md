Fixed
- `tm architect serve`/`poll` find panes when the process has no UTF-8 locale: tmux runs with `-u`, so it no longer rewrites the TAB in `list-panes -F` output to `_`. A `list-panes` row the parser cannot read is kept in `Listing::unreadable` and logged at warn level instead of dropped silently (#11543).
