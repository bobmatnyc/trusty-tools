Changed
- `tm fleet init` seeds `notify-supervisor.sh` re-running itself under `timeout 4 /bin/bash "$0"`, never a direct `#!` exec (D73).
