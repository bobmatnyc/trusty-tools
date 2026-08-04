Changed

- `framework-manifest.toml` is now the single authority for agent AND skill
  bundling ([#4765](https://github.com/bobmatnyc/trusty-tools/issues/4765))
  - each gated agent declares its own `markers` in the manifest; the
    `LANGUAGE_ENGINEERS` and `PLATFORM_AGENTS` Rust tables are gone, so an
    entry's category and the condition that deploys it are one declaration
  - a new `[skill_categories]` section declares the bundled skill roster; a
    bundled skill nobody declares is a hard error, matching the agent side
  - a gated entry declaring no markers is rejected rather than silently
    becoming undeployable
  - stack and platform detection now share one probe budget and one
    workspace-member resolution per call instead of one of each
  - `references/agents.md` gains a **Deploys When** column rendered from the
    manifest, and `references/skills.md` renders the declared skill roster
  - the bundled `tm`, `tm-delegation-patterns`, and `agent-delegation`
    documents no longer restate the roster or its gates — they point at the
    manifest and at the generated reference
