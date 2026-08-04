Added

- bundled `framework-manifest.toml` — the framework tier of the existing
  `manifest.toml` format — now declares which agents deploy, replacing the
  computed "everything not in `LANGUAGE_ENGINEERS`" rule
  (closes [#4760](https://github.com/bobmatnyc/trusty-tools/issues/4760))
  - four deployment categories: `universal` (no detection), `language`,
    `framework`, and `platform` (marker-gated), plus `deprecated`
  - a missing, malformed, or non-exhaustive framework manifest fails loudly
    rather than falling back to deploying everything or nothing
  - `tm generate capabilities` now sources the agent reference's deployment
    category from the manifest
- new `elixir-engineer` agent, gated on `mix.exs`, covering general Elixir and
  OTP work — supervision trees, GenServer processes, Ecto, ExUnit
