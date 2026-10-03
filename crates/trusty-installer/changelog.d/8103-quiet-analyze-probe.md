Fixed
- `tctl status` no longer prints trusty-analyze's startup lines (the missing-API-key WARN and the serving banner) when its probe starts the server on demand. The started server's stderr goes to `trusty-analyze.stderr.log` beside its socket (#8103).
