Fixed

- `tm` and `tm launch` no longer run `git init` in a workspace parent. The auto-init
  decision now runs the same downward child-repository scan the `CLAUDE.md` seed guard
  uses, and refuses — naming the child repository, or what stopped the scan — when a
  repository is found at any depth beneath the directory or the scan cannot finish.
  Both scan refusals name the remedy — run `git init` in the directory yourself when it
  really is a single project — instead of the "run tm from a project directory" trailer
  that only fits the `$HOME` and `/` refusals
  ([#7749](https://github.com/bobmatnyc/trusty-tools/issues/7749))
