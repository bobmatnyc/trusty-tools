Fixed

- **Issue #4823 — `service install` no longer discards a deliberate
  `--no-auto-discover`.** `trusty-search service install` regenerated the
  launchd unit from a fixed template, so an operator who disabled the
  auto-discovery scan lost that setting on the next install and could not make
  it durable by any supported means. Three changes:
  - `service install` accepts `--no-auto-discover`, which writes the flag into
    the generated unit's `ProgramArguments`. Re-running `service install`
    **preserves** the setting and says so; re-enabling the scan now requires an
    explicit `service install --auto-discover`, so a capability change is never
    a silent side effect of reinstalling.
  - Operator tunables (`TRUSTY_DEVICE`, `TRUSTY_MEMORY_LIMIT_MB`, and the rest
    of `PERSISTED_ENV_VARS`) that the installed unit already carried now survive
    regeneration instead of being blanked whenever `service install` runs from a
    shell that exports none of them. An exported value still wins.
  - `--no-auto-discover` / `TRUSTY_NO_AUTO_DISCOVER` accepts `1`/`true`/`yes`/
    `on` and `0`/`false`/`no`/`off` (case-insensitive). Previously the env var
    went through clap's strict `FromStr<bool>`, so the `=1` spelling documented
    in the README and the #314 changelog — and already present in many
    `daemon.env` files — was **rejected** and aborted daemon startup:

        error: invalid value '1' for '--no-auto-discover': [possible values: true, false]

    An unrecognised spelling is still an error rather than a silent `false`, so
    a typo fails loudly instead of quietly re-enabling the scan. The suppression
    itself travels as a CLI flag, never as a `TRUSTY_NO_AUTO_DISCOVER` entry in
    the generated plist, so a generated unit can never carry a value the daemon
    would refuse to parse.
