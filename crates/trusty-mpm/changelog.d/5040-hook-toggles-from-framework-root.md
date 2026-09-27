Fixed

- The project hook-set resolver behind `tm doctor --fix`, `tm validate` and
  the resume merge now reads `[pm] prompt_self_improvement` from the same
  framework-root `config.toml` as `[hooks] prompt_context` and
  `[divert] enabled`. Before, that one toggle was read through `$HOME`
  separately, so a caller supplying its own framework layout still got the
  host's answer for the `Stop`/`SubagentStop` capture groups. Production
  output is unchanged: both reads resolve to `~/.trusty-mpm/config.toml`.
