Fixed

- `tm hook --pm-guard` now refuses a sibling credential call written as an argv list, the way a script body runs it: `subprocess.run(["op", "read", ref])`, a tuple, single quotes, one element per line, flags between (`["op", "--account", "work", "read", ref]`), and the same lists for `gh auth token`, `gcloud secrets … access`, `aws configure get`/`export-credentials` and `security -i` (Refs [#10734](https://github.com/bobmatnyc/trusty-tools/issues/10734)). The words must sit in one list, call or line, so a quoted `op` dict key or prose naming `op` before a later `read` still passes.
