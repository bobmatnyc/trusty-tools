Fixed

- `tagent --version` now names the commit the binary was built from, and says the same thing every time ([#4260](https://github.com/bobmatnyc/trusty-tools/issues/4260))
  - It used to print `trusty-agents v0.39.0 build #2435`, where the number came from `.trusty-agents/state/build.json` and was incremented by the `--version` path itself. The same installed binary answered `build #2435`, then `build #2440`, so the string could not be quoted as install evidence — and it read exactly like a build id, which is how a ~3.5h-stale binary was reported as a code bug on 2026-07-28.
  - `--version` now prints `trusty-agents v0.39.0 (fa4b02a30, 2026-08-31T09:12:44-04:00)` — crate version, short commit SHA, and that commit's date, all baked in at compile time. It touches no disk and creates no state directory, so it is safe to run from a script or a read-only cwd.
  - A build from a dirty working tree marks the SHA: `(fa4b02a30-dirty, …)`.
  - `GET /api/health` gained `commit`, `commit_full`, `commit_date`, and `dirty`, so a running daemon is identifiable over HTTP rather than by comparing file mtimes. The Svelte header picks up `commit` with no UI change.
  - The per-invocation counter is not gone — it still disambiguates concurrent runs in a merged log and stamps telemetry filenames — but the startup log line now labels it `run #N`, and it appears in neither `--version` nor the REPL's `/version`.
