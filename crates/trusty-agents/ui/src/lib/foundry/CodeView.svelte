<script>
  // Canonical source: docs/design/UI/design-system/components/CodeView.svelte.
  // Vendored byte-for-byte into crates/trusty-agents/ui/src/lib/foundry/.
  /*
   * Why: one highlighted-source block for the agents GUI file viewer and the
   * search dashboard's hit viewer.
   * What: renders pre-highlighted `html` (from `renderCode`) in a `pre.file-code`.
   * With `startLine`, a line-number gutter starting there sits beside it.
   * Colours come from `--code-accent` / `--code-string`, falling back to the
   * Foundry `--trusty-*` tokens.
   * Test: `HitViewer.test.js` (search) and `FileViewer.test.ts` (agents).
   */
  import { gutterNumbers } from './codeView.js';

  /** @type {{ html: string, startLine?: number | null, class?: string, label?: string }} */
  let { html, startLine = null, class: className = '', label = 'File source' } = $props();

  let numbers = $derived(startLine == null ? '' : gutterNumbers(startLine, html));
</script>

{#if startLine == null}
  <pre class="file-code {className}" aria-label={label}><code>{@html html}</code></pre>
{:else}
  <div class="numbered">
    <pre class="gutter {className}" aria-hidden="true">{numbers}</pre>
    <pre class="file-code {className}" aria-label={label}><code>{@html html}</code></pre>
  </div>
{/if}

<style>
  .numbered { display: grid; grid-template-columns: auto minmax(0, 1fr); }
  .gutter {
    margin: 0;
    padding-right: .75rem;
    text-align: right;
    user-select: none;
    opacity: .5;
    border-right: 1px solid var(--code-border, var(--trusty-border));
  }
  .numbered .file-code { margin: 0; padding-left: .75rem; overflow-x: auto; }
  .file-code :global(.hljs-keyword), .file-code :global(.hljs-built_in) { color: var(--code-accent, var(--trusty-accent)); font-weight: 600; }
  .file-code :global(.hljs-string), .file-code :global(.hljs-title) { color: var(--code-string, var(--trusty-success)); }
  .file-code :global(.hljs-comment) { opacity: .55; }
  .file-code :global(.hljs-number), .file-code :global(.hljs-literal) { color: var(--code-accent, var(--trusty-accent)); }
</style>
