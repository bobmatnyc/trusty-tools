<script>
  /*
   * Why: a search hit's snippet is cut at 320 characters; the operator needs
   * the hit's whole line range, highlighted, to judge it.
   * What: a modal dialog showing the hit's indexed lines (`content`, which the
   * daemon returns for every hit) through the shared Foundry `CodeView`, with
   * a gutter numbered from `start_line`. Escape, the close button and a
   * backdrop click all close it; focus moves to the close button on open and
   * Tab / Shift+Tab cycle inside the dialog (modal focus trap).
   * The full file and its diff are not shown: no route the dashboard can
   * reach returns either (see the crate changelog for the proposed route).
   * Test: `HitViewer.test.js`.
   */
  import CodeView from '@foundry/CodeView.svelte';
  import { escapeHtml } from '@foundry/codeView.js';
  import { highlightCode } from '../highlight.js';

  /** @type {{ hit: any, onclose: () => void }} */
  let { hit, onclose } = $props();

  const uid = $props.id();
  const FOCUSABLE = 'button:not([disabled]), [href], input, select, textarea, [tabindex]:not([tabindex="-1"])';
  let closeButton = $state(null);
  let dialog = $state(null);

  let displayPath = $derived(hit.path || hit.file || hit.id);
  // Plain text first; the highlighted HTML replaces it once the grammar loads.
  let html = $state('');
  $effect(() => {
    const source = hit.content ?? '';
    let live = true;
    html = escapeHtml(source);
    highlightCode(source, hit.file || hit.path || '').then((h) => {
      if (live) html = h;
    });
    return () => {
      live = false;
    };
  });

  $effect(() => {
    closeButton?.focus();
  });

  /**
   * Why: `aria-modal` tells assistive tech the page behind is inert, so focus
   * must not reach it either.
   * What: Tab from the last focusable element (or from outside the dialog)
   * goes to the first; Shift+Tab from the first (or outside) goes to the last.
   */
  function trapTab(e) {
    const items = dialog ? [...dialog.querySelectorAll(FOCUSABLE)] : [];
    if (items.length === 0) return;
    const first = items[0];
    const last = items[items.length - 1];
    const inside = dialog.contains(document.activeElement);
    if (e.shiftKey && (!inside || document.activeElement === first)) {
      e.preventDefault();
      last.focus();
    } else if (!e.shiftKey && (!inside || document.activeElement === last)) {
      e.preventDefault();
      first.focus();
    }
  }

  function onKeydown(e) {
    if (e.key === 'Escape') {
      e.preventDefault();
      onclose();
    } else if (e.key === 'Tab') {
      trapTab(e);
    }
  }
</script>

<svelte:window onkeydown={onKeydown} />

<!-- The backdrop is a mouse convenience; Escape and the close button are the
     keyboard paths. -->
<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div class="modal-backdrop" onclick={(e) => e.target === e.currentTarget && onclose()}>
  <div class="modal viewer" role="dialog" aria-modal="true" aria-labelledby={`${uid}-title`} bind:this={dialog}>
    <div class="modal-header">
      <div class="title-block">
        <div id={`${uid}-title`} class="modal-title text-mono">{displayPath}</div>
        <div class="meta text-sm">
          {#if hit.index_id}<span class="badge badge-info">{hit.index_id}</span>{/if}
          <span>Lines {hit.start_line}–{hit.end_line}</span>
          {#if hit.function_name}<span class="badge badge-muted">{hit.function_name}</span>{/if}
          {#if hit.language}<span class="text-muted">{hit.language}</span>{/if}
        </div>
      </div>
      <button class="btn btn-sm" bind:this={closeButton} onclick={onclose} aria-label="Close file viewer">
        Close
      </button>
    </div>
    <!-- Focusable so a keyboard user can scroll long lines and ranges. -->
    <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
    <div class="modal-body code-body" role="region" aria-label="Indexed lines" tabindex="0">
      {#if hit.content}
        <CodeView {html} startLine={hit.start_line ?? 1} class="code" />
      {:else}
        <p class="text-muted text-sm">The daemon returned no text for this hit.</p>
      {/if}
    </div>
    <div class="modal-footer note text-xs text-muted">
      Indexed lines only — the search daemon has no route that returns the whole file or its diff.
    </div>
  </div>
</div>

<style>
  .viewer {
    max-width: min(1100px, 94vw);
    display: flex;
    flex-direction: column;
  }
  .title-block {
    min-width: 0;
  }
  .title-block .modal-title {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .meta {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--trusty-space-2);
    margin-top: var(--trusty-space-1);
    color: var(--trusty-text-secondary);
  }
  .code-body {
    overflow: auto;
    background: var(--trusty-content-bg);
  }
  .code-body :global(.code) {
    margin: 0;
    font-family: var(--trusty-mono);
    font-size: var(--trusty-fs-xs);
    line-height: 1.6;
    white-space: pre;
  }
  .note {
    justify-content: flex-start;
  }
</style>
