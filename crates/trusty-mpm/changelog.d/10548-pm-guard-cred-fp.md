Fixed
- `tm hook --pm-guard` no longer refuses a script whose `case` arm lists path patterns (`a | b)`), or a script that names a directory or a non-executable file where a command would go, so `scripts/select-test-crates.sh`, `scripts/select-test-crates_selftest.sh`, `scripts/detect-docs-only.sh` and `scripts/package_content.sh` run again (#10548).
- A method call on a value named `token`, `secrets` or `credentials` in embedded program text (`token.strip()`) is no longer read as a secret-bearing file, so `scripts/check_rustdoc_links.sh` runs again (#10548).
- A refusal of a script that runs another script now names the inner script, instead of saying the outer one "does not exist" (#10548).
- `cargo`, `rsync`, `cp`, `mv`, `ln`, `mkdir` and `touch` given two adjacent `$`-expansions (`cargo test -p "$c" "$t" --no-fail-fast`) are no longer refused as a `gh api` secret DELETE (#10548).
