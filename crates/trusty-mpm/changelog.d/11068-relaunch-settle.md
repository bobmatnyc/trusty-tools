Fixed

- `tm fleet relaunch` no longer fails with "still holds a `claude`, or cannot say" when a loaded host has not yet settled the pane after the old `claude` stops. It now reads the pane every 100 ms for up to 5 s and goes on at the first read that shows no `claude`; a pane that still holds one after 5 s still refuses, and nothing new is started (#11068).
