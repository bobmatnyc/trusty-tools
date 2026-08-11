Changed

- `audit` now passes `--synthesize` to `trusty-review report`, so the due-diligence report carries a written executive summary, top-risk rationale, and RED/AMBER finding prose. Until now it passed only `--analyze`, and every audit report ever produced was fully deterministic (#5454).
- `audit` requires `OPENROUTER_API_KEY` and checks it before stage 1. An unset or blank key fails immediately, naming the variable and how to set it, rather than after a multi-minute sweep.
- A failed render now prints the exact command to re-run just the render; `manifest.toml` is written before the renderer is called, so nothing collected is lost.
- `audit` requires `trusty-review` 0.15.0 or newer, and checks the installed version before stage 1. The two are installed separately through PATH, and an older renderer accepts `--synthesize`, degrades to a narrative-free report whenever the model call fails, and still exits 0 — so the audit used to report a clean pass over a report with no written analysis.
- `audit` now checks the report `trusty-review` wrote, not just the child's exit status. A report carrying no written analysis fails the run and names the renderer upgrade that fixes it.
