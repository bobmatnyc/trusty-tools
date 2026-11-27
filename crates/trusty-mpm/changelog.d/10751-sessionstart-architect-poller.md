Fixed

- A Claude `SessionStart` in an Architect's directory now links to the Architect instead of logging an ERROR that two managed sessions share the cwd: the Architect's `-poll` and `-collector` helpers run no `claude` and no longer count toward that collision. Two `claude` sessions in one cwd still skip attribution and log the ERROR (#10751).
