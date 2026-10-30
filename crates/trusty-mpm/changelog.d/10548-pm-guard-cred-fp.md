Fixed
- `tm hook --pm-guard` no longer refuses a script whose `case` arm lists path patterns (`a | b)`), or one that names a directory where a command would go, so `scripts/detect-docs-only.sh` runs again (#10548). Only a `case WORD in` header in command position opens an arm list.
- A script that names a missing script only on a comment line is no longer refused for it; a `#` line inside an open quote is code, not a comment, so a command on it is still judged (#10548).
- A method call on a value named `token`, `secrets` or `credentials` in embedded program text (`token.strip()`) is no longer read as a secret-bearing file, so `scripts/check_rustdoc_links.sh` runs again; shell arguments such as `cat 'token.get(1)'` still refuse (#10548).
- A refusal of a script that runs another script now names the inner script, instead of saying the outer one "does not exist" (#10548).
- `cargo`, `rsync`, `cp`, `mv`, `ln`, `mkdir` and `touch` given two adjacent `$`-expansions (`cargo test -p "$c" "$t" --no-fail-fast`) are no longer refused as a `gh api` secret DELETE, unless the command can make that program run something else (`hash`, `PATH=`, `source`, `.`, `alias`, a function, `eval`, `cd`) (#10548).
- A script body whose `#` line sits inside an open quote (`echo "a` then `#"; security find-generic-password -w`) is now refused; the line was blanked as a comment before (#10548).
