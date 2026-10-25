Fixed
- `tm hooks clean` without `--path` no longer walks `~/Documents`, `~/Desktop` and the other synced home folders, or onto another device, and `tm doctor`'s machine-wide settings sweep now uses that same shared walk ([#10267](https://github.com/bobmatnyc/trusty-tools/issues/10267))
