Changed
- `frontmatter`, `entity`, `schema`, `validate` and `reconcile` now live in trusty-okf and are re-exported under their old `trusty_kb::` paths until trusty-kb is removed (#10746). trusty-kb now depends on trusty-okf.
- `frontmatter::parse` and `Entity::from_content` return `trusty_okf::OkfError` instead of `anyhow::Error`; `?` in an `anyhow` function still works.
- `kb_validate` findings gain `severity` (`error`, `warning` or `info`), `concept` and `spec_ref` fields beside `path`, `kind` and `message`.
- Inverse edges for reconciliation come from the profile's edge table (`Profile::edge_pairs`), which the default profile sets to the existing `EDGE_PAIRS`.
