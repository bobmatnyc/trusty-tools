Fixed

- `POST /indexes` on a root whose `.trusty-search/` the daemon cannot write now answers 403 `permission denied: …`, naming the directory and the `colocated: false` alternative, instead of 500 `corpus open failed`. The refusal happens before anything is built or recorded, so no registry entry or directory is left behind and a retry with `colocated: false` succeeds ([#8147](https://github.com/bobmatnyc/trusty-tools/issues/8147))
