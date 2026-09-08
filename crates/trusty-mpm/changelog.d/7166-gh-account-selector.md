Added

- `tm` accepts an explicit GitHub account selector for a managed clone and
  session: `--account <login>` (a global flag, so `tm --account bob-duetto
  <url>`, `tm run --account bob-duetto <owner>/<repo>`, and `tm register
  --account bob-duetto <owner>/<repo>` all work), or the terser
  `<login>@<owner>/<repo>` shorthand on the bare/`run` managed-repo forms
  (`tm bob-duetto@duettoresearch/poc-hotel-supply`). Fixes clones of a
  private repo visible only to a non-default `gh` account when every other
  credential path on the machine (an exported `GH_TOKEN`, git's global `!gh
  auth git-credential` helper, a fixed SSH key) resolves to a different
  identity (#7166).
  - The daemon's `inproject` base clone (`git clone --no-local`) authenticates
    as the selected account: its token is minted via `gh auth token -u
    <account>` and reaches `git` only through the child process's
    environment (`GH_TOKEN`/`GH_USER`) — never argv or the clone URL — with
    every inherited `GH_TOKEN`/`GITHUB_TOKEN`/`GH_CONFIG_DIR`/`GH_USER`
    removed first, so an ambient token belonging to a different account
    cannot outrank the selection. An account not logged into `gh` fails
    loud, by name, before any clone is attempted.
  - The selected account is persisted onto the project's registry-B
    record (`gh_account`), the same field `tm projects register
    --gh-account` and `tm projects show`/`status` already read and display,
    so later spawns, fetches, pushes, and `gh` calls made from inside the
    session reuse it automatically.
