Fixed

- A bare `trusty-audit` in a directory with no `engagement.toml` now sets the
  engagement up instead of registering targets against one that does not exist.
  It asks for the OpenRouter key first, writes `engagement.toml` at mode 0600
  with an exact version pinned per tool, preflights all four pinned tools,
  installs them, and asks what the audit covers LAST. Registration going first
  is what produced the reported launch — targets registered, `Tools: 0/4
  installed`, a command named to run next, and no key prompt at any point
  ([#5970](https://github.com/bobmatnyc/trusty-tools/issues/5970))
- A cold start whose `engagement.toml` cannot be written now stops and says so,
  naming the file and stating that the key was not saved. It used to be
  unreachable: nothing wrote the file, so the three gates that hit its absence
  each degraded quietly and none of them named it
  ([#5970](https://github.com/bobmatnyc/trusty-tools/issues/5970))
