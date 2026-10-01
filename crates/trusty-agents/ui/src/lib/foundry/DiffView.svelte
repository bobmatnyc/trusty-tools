<script>
  // Canonical source: docs/design/UI/design-system/components/DiffView.svelte.
  // Vendored byte-for-byte into crates/trusty-agents/ui/src/lib/foundry/.
  /*
   * Why: one unified-diff block for the agents GUI file viewer and any
   * dashboard that can fetch a diff.
   * What: one `div` per diff line inside a `pre`, tinted by `diffLineKind`.
   * Colours come from `--code-added-bg` / `--code-removed-bg` /
   * `--code-accent`, falling back to the Foundry `--trusty-*` tokens.
   * Test: `FileViewer.test.ts` in crates/trusty-agents/ui/src/components/.
   */
  import { diffLineKind } from './codeView.js';

  /** @type {{ diff: string, class?: string, label?: string }} */
  let { diff, class: className = '', label = 'File diff' } = $props();

  let lines = $derived(diff.split('\n').map((text) => ({ text, kind: diffLineKind(text) })));
</script>

<pre class={className} aria-label={label}>{#each lines as line}<div class:diff-added={line.kind === 'added'} class:diff-removed={line.kind === 'removed'} class:diff-hunk={line.kind === 'hunk'}>{line.text || ' '}</div>{/each}</pre>

<style>
  .diff-added { background: var(--code-added-bg, var(--trusty-success-soft)); }
  .diff-removed { background: var(--code-removed-bg, var(--trusty-danger-soft)); }
  .diff-hunk { color: var(--code-accent, var(--trusty-accent)); }
</style>
