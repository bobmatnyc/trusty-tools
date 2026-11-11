Fixed
- `tm hook --pm-guard` no longer refuses `for c in a b; do cargo test -p "$c" "$t" --no-fail-fast; done` as a `gh api` secret DELETE. A `for` loop whose body is one plain `cargo`, `rsync`, `cp`, `mv`, `ln`, `mkdir` or `touch` command, with a lower-case loop variable and no redefining word, is read like the same command alone; every other loop still judges each operand pair (#10548).
