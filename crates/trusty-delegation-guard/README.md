# trusty-delegation-guard

Wire types for the trusty-mpm focused delegation guard, a SHADOW-only
observer of Agent/Task dispatches ([#10892](https://github.com/bobmatnyc/trusty-tools/issues/10892)).

The `types` feature (default, and the only feature today) defines:

- `dg.event.v1` — one observation of a dispatch at the contract or launch
  boundary. Raw session and tool-use ids are stored only as hashes; the prompt
  is stored as a redacted excerpt of at most 8 KiB plus a digest.
- The per-project policy bundle. A bundle refuses rules from another project.
- `dg.verdict.v1` — the evaluator's answer. An error never becomes `allow`.
- Canonical hashing and the dedupe key.

Every parser rejects unknown keys. The crate has no inference, HTTP or model
dependency. Design decisions are recorded in the crate docs (`src/lib.rs`).

Test fixtures under `tests/fixtures/` are synthetic. The `labels/` directory
is read only by tests and is excluded from the package.
