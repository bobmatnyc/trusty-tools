Fixed

- **The `clustering` feature did not compile.** `concept_cluster`'s `chunk_with` test helper builds a `CodeChunk` literal and predates the `on_branch` and `archive_reason` fields; `#[serde(default)]` on those two covers deserialization only, so the literal failed `E0063`. The module sits behind the off-by-default `clustering` feature and no CI job passes that flag, so its 7 tests had never run. The helper now names both fields (closes [#5931](https://github.com/bobmatnyc/trusty-tools/issues/5931))
