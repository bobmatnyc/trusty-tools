Fixed

- A bundled skill's `scripts/*` and non-`.md` `references/*` files now reach every deployed tier byte-identical: the framework skill source writes `scripts/*` as mode 755, `tm doctor --fix-skills` repairs a script executable, the drift reference and the launch-time staleness probe cover every carried file (not only `references/*.md`), and the skill-source prune removes a stale script or `.yml` it wrote while leaving any file it never wrote (Refs [#10211](https://github.com/bobmatnyc/trusty-tools/issues/10211))
