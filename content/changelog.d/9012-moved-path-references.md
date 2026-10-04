Changed
- The four output styles, `BASE_SM.md` and `WHAT-IS-TRUSTY-MPM.md` point an agent inside the trusty-tools repo at `content/instructions/docs/WHAT-IS-TRUSTY-MPM.md`, and `mpm-skills-manager` names `content/skills/` as the bundled skill source, where #9012 moved them; the deployed path `~/.trusty-mpm/framework/docs/` is unchanged.
- `tm-capabilities` references regenerated: the bundled docs ship with the content, and the skill catalog is read from the content's `skills/` (#9012).
