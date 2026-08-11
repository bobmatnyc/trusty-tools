Changed

- `audit` now passes `--synthesize` to `trusty-review report`, so the due-diligence report carries a written executive summary, top-risk rationale, and RED/AMBER finding prose. Until now it passed only `--analyze`, and every audit report ever produced was fully deterministic (#5454).
- `audit` requires `OPENROUTER_API_KEY` and checks it before stage 1. An unset or blank key fails immediately, naming the variable and how to set it, rather than after a multi-minute sweep.
- A failed render now prints the exact command to re-run just the render; `manifest.toml` is written before the renderer is called, so nothing collected is lost.
