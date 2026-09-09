Added

- `engagement::create` mints a whole engagement in one call, replacing the
  hand-edited template: it writes `engagement.toml` at mode 0600 carrying the
  operator's OpenRouter key, the audit window and a freshly generated ed25519
  signing seed, plus the retained public half beside it in `retained.pub` — the
  file `trusty-audit verify --public-key` reads and the one that never travels
  with the package. Nothing about it reaches the network: the key is the
  operator's, conveyed out of band, the keypair comes from the operating
  system's randomness, and the tool pins come from the template rather than
  from a release lookup. It reuses `config::generate_for_new_engagement` and
  the new `config::with_engagement_identity` rather than adding a third writer
  of a plaintext credential, so the previous engagement's board credentials,
  signing key and client labels are dropped and reported by name. A directory
  that already holds an engagement is refused rather than overwritten —
  replacing the signing key would orphan the public half the auditor
  retained — and `force` is how an operator says they meant it. A mint that
  cannot write the retained public key removes the config it just wrote instead
  of leaving an engagement whose signatures nothing could ever check.
- The engagement config accepts an `[audit] window_weeks` key, defaulting to 52
  so a config written before it existed still loads and still means one year.
  A declared `0` is refused at parse time rather than accepted: a zero-week
  engagement sweeps no history and would still exit 0. #5482 is the separate
  work of making `tga` honour the value; until that lands it is inert.
