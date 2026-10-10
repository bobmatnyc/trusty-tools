Fixed
- The `tm secrets exec` helper sets close-on-exec on the report and token pipes it inherits, so a trusty-secrets server it auto-spawns no longer holds the report pipe open (#9621).
