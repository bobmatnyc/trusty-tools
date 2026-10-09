Fixed
- `tm doctor --fix`'s machine-wide settings scan takes its home root from its caller; its test now walks a temp home instead of the real `$HOME`, which ran ~30 min on iCloud-backed directories ([#9293](https://github.com/bobmatnyc/trusty-tools/issues/9293))
