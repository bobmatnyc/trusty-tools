Added

- New crate `trusty-delegation-guard` with a `types` feature (default) for the SHADOW-only focused delegation guard: the `dg.event.v1` event, the per-project policy bundle, the `dg.verdict.v1` verdict, canonical hashing, the dedupe key, credential redaction with an 8 KiB excerpt cap, strict parsing that rejects unknown keys, type-level per-project isolation, and nine synthetic fixtures with labels kept out of the package. It has no inference, HTTP or model dependency. ([#10892](https://github.com/bobmatnyc/trusty-tools/issues/10892))
