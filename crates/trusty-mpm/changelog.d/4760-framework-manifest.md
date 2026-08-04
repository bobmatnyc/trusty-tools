Added

- bundled `framework-manifest.toml` — the framework tier of the existing
  `manifest.toml` format — now declares which agents deploy, replacing the
  computed "everything not in `LANGUAGE_ENGINEERS`" rule
  (closes [#4760](https://github.com/bobmatnyc/trusty-tools/issues/4760))
  - four deployment categories: `universal` (no detection), `language`,
    `framework`, and `platform` (marker-gated), plus `deprecated`
  - `gcp-ops` and `vercel-ops` are now platform-gated and no longer deploy to
    projects with no GCP or Vercel marker — an intended behavior change
  - the deprecated `ops` agent no longer deploys; it was marked deprecated in
    delegation prose with no code-level effect and still reached every roster
  - a missing, malformed, or non-exhaustive framework manifest fails loudly
    rather than falling back to deploying everything or nothing
  - `tm generate capabilities` now sources the agent reference's deployment
    category from the manifest
