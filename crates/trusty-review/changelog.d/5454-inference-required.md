Breaking

- `report` now always synthesizes, and a synthesis that produces no verified prose fails the run instead of writing a deterministic-only report (#5454).
- `--synthesize` is accepted but ignored, and prints a deprecation line — the flag stays parseable so scripts and older `tga` invocations keep working.
- `report` checks `OPENROUTER_API_KEY` before reading the manifest, so a missing credential costs nothing but the error.
- `Synthesizer::synthesize` returns `Result<Synthesis, SynthesisError>`; `SynthesisStatus`, `Synthesis::unavailable`, and `Synthesis::is_available` are removed, so the type can no longer represent a failed pass.
- `Reporter::write` rejects a model carrying no synthesis with the new `ReportError::SynthesisRequired`.
- The deterministic Executive Summary composition from #5374 is kept: it now fills §2 when the numeric guardrail rejects the model's summary.
