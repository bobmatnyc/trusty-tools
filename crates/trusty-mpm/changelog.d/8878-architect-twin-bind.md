Fixed

- pm-guard no longer refuses a quoted here-document append whose body holds a `(` before an apostrophe when a later line opens a multi-line `"…"` string (the Architect's `cat >> local/actions.md <<'EOF'` then `git commit -qm "…"`); the same here-document aimed at a trust anchor is still judged as an anchor write (#8878).
- An Architect `claude` started by `tm connect`, `tm launch`, `tm session start` or the daemon client is now bound with a launch record, as `tm fleet init` and `tm fleet relaunch` already did, so pm-guard recognises it; one that cannot be bound prints a warning naming `tm fleet relaunch`. Every launch record is written by one function, `bind_architect` (#8878).
