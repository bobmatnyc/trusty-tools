Fixed
- `tm daemon` and `tm sm serve --stdio` now settle buffered action events (send or spool) on SIGHUP, the same path as SIGTERM and SIGINT, before they exit. A SIGHUP inherited as ignored (`nohup`) stays ignored.
