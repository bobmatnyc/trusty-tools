Fixed

- The `output_style_staleness` and `output_style` checks no longer tell the operator to run `tm install`, which has no output-style step — the deployed file kept its mtime across a full install run. Both now name `tm doctor --fix --yes`, and the drift scan behind the report is the same one the repair acts on ([#5866](https://github.com/bobmatnyc/trusty-tools/issues/5866))
