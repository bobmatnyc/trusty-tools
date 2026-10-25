Changed
- `tm fleet init` seeds `notify-supervisor.sh` re-running itself under `timeout 4 /bin/bash "$0"`, never a direct `#!` exec (D73).
- An untouched deployed `architect-project-provision` skill from the previous release is recognised and replaced by the D73 text on the next launch.
