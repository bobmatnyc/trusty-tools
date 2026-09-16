Added
- `tcode run-task --pm-model <SLUG>` (env fallback `TCODE_PM_MODEL`) overrides
  the TOP-LEVEL agent's own model, the half `--engineer-model` never covered.
  Precedence is flag > env > the agent's front-matter `model:` > the built-in
  default; short aliases are accepted and normalised. It reaches both the
  daemon `task.run` path (as a new `pm_model` request field) and
  `--legacy-in-process`.
