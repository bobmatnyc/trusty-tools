Changed

- `tm` and `tm launch` in a git repository with no origin remote now start a
  session in that checkout instead of stopping at "no git origin remote found"
  / "not auto-managing this project"
  ([#6276](https://github.com/bobmatnyc/trusty-tools/issues/6276)). A two-line
  notice still says there is no remote and that no managed clone is made, and
  then the session runs. This is the end-state #6274's auto-`git init` left
  open: a first-ever `tm` run in a plain directory created the repository and
  the very next step refused it. Repositories that HAVE an origin remote are
  unchanged — a GitHub remote still gets the protected managed clone, and a
  non-GitHub remote is still refused with the live checkout untouched.
