Fixed

- The daemon's pr-cleanup sweep now backs off instead of spawning one doomed `gh pr view` per pending pull request on every tick when `gh` has no usable credential. Two consecutive authentication failures suspend the sweep's `gh` calls for five minutes, doubling per further failure to a one-hour ceiling, and any answer at all clears the strikes. `/health` gains a `degraded` list naming the suspended subsystem and the `gh auth login` remedy, which `tm status` prints (refs [#8058](https://github.com/bobmatnyc/trusty-tools/issues/8058))
