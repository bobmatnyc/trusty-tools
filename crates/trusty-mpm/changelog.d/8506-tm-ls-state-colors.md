Added

- `tm ls` colors each session row by state again, in both the static table and the session TUI: active green, stopped yellow, dead (errored, unresumable, or a deleted slot) red, attached bold cyan, provisioning blue, decommissioned dim gray; an unrecognised state stays uncolored. The numbered picker uses the same mapping, so its stopped rows turn yellow instead of dim. Colors appear only when stdout is a terminal and `NO_COLOR` is unset; `--json` and piped output are unchanged ([#8506](https://github.com/bobmatnyc/trusty-tools/issues/8506))
