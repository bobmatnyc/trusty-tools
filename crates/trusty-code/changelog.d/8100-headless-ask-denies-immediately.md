Fixed
- A headless `task.run` with an `ask` permission rule now denies immediately instead of waiting out the 300 s ask timeout (#8100). The gate asks the event sink whether anything is consuming the session's events; a run nobody is watching is headless even though the daemon always mints permission-broker state for it. An attached client still gets the full prompt round trip.
