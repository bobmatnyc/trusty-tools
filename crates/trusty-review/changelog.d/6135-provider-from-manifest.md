Added

- `report --manifest` resolves the provider and per-role models from the
  manifest's new `[inference]` section, ahead of this host's environment and
  `~/.config/trusty-review/config.toml`. Precedence per field is CLI flag >
  manifest > env var > config file > built-in default, so a delivered audit
  package re-renders on the provider that produced it — the failure this
  replaced was a June-dated local `provider = "bedrock"` hijacking an OpenRouter
  engagement's render. A manifest with no `[inference]` section (every manifest
  written before this) resolves exactly as it did before. The section carries
  identity only: a credential still comes from the environment, and a render
  whose declared provider has no key stops before any repository is walked, with
  the provider named in the message.
- The report states which models ran. Its Report Metadata table gains an
  `Inference models` row and the JSON twin an `inference` object, both listing
  provider and per-role model — and both showing `requested → ran` wherever the
  resolver adjusted an id.
