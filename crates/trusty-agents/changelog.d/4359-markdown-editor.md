Added

- **Markdown editor component for the trusty-agents UI (issue [#4359](https://github.com/bobmatnyc/trusty-tools/issues/4359)).** `MarkdownEditor.svelte`
  is a reusable, controlled CodeMirror 6 editing surface — `value` in, `change`
  and `save` out, `readonly` to make it view-only — with markdown syntax
  highlighting themed from the app's own Foundry tokens, `Mod-S` to save, and no
  knowledge of routes or persistence, so any markdown workflow can mount it.
  `ProjectFilesPanel.svelte` wires it into the Projects surface: it browses a
  registered project root, opens markdown documents and saves them back through
  the per-project file routes ([#4357](https://github.com/bobmatnyc/trusty-tools/issues/4357)),
  listing non-markdown files but leaving them unopenable until document-type
  gating lands ([#4360](https://github.com/bobmatnyc/trusty-tools/issues/4360)).
  CodeMirror is loaded through a dynamic `import()` on first open, so the ~176 kB
  gzipped editor never reaches the startup path of a user who does not open a
  document.
