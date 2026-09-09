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
  retained — and `force` is how an operator says they meant it.
- The two halves of an engagement's keypair are published as one unit. The
  config carrying the private seed and the `retained.pub` holding its public
  half are written to temporaries first and then renamed into place, public half
  first, so a mint that fails leaves the directory exactly as it found it — a
  `--force` remint that cannot write one half leaves the previous engagement
  whole rather than destroying it, and a config on disk is always accompanied by
  the public half of the seed it carries. The check for an existing engagement
  and the writes now run under that config's exclusive lock, so two mints aimed
  at one directory cannot both pass the check.
- The engagement config accepts an `[audit] window_weeks` key, defaulting to 52
  so a config written before it existed still loads and still means one year.
  A declared `0` is refused at parse time rather than accepted: a zero-week
  engagement sweeps no history and would still exit 0. #5482 is the separate
  work of making `tga` honour the value; until that lands it is inert.
