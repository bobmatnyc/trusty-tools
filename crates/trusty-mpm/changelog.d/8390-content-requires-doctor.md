Added
- Content assets may declare an advisory `requires:` frontmatter block mapping a harness to a SemVer requirement (`requires: {tm: ">=1.9.0"}`). The new `tm doctor` row `content_requires` warns, naming each asset whose `tm` requirement this binary does not meet or whose block it cannot read; the asset still deploys and runs, and the row is never FAIL (ADR-0064, #8390).
