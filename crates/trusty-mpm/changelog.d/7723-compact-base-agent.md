Added

- New bundled skill `self-improvement-loop` carries the self-analysis
  reporting protocol and the fast-loop hypothesis record moved out of the
  resident `BASE-AGENT.md` — on-demand only, not declared in any agent's
  `skills:` frontmatter, so it is loaded once at report time instead of
  preloaded on every turn (#7723).
