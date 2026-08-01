<script lang="ts">
  /**
   * Why (#4359): writing tools are an M2 pillar, and `ui/package.json` had no
   * editing surface at all — markdown authoring meant leaving the app. This is
   * that surface: one reusable component owning the editing experience and
   * nothing else, so every markdown workflow in trusty-agents (project docs
   * today, agent personas and canvas documents later) shares one behaviour
   * instead of each growing its own textarea.
   *
   * WHY CODEMIRROR 6 (the #4359 evaluation, recorded here so it isn't
   * re-litigated): the alternatives were Monaco (a full IDE — far more weight
   * and DOM than a document editor needs), ProseMirror/Milkdown/Tiptap (WYSIWYG
   * editors that own the *document model*; they round-trip markdown through an
   * AST, so byte-for-byte fidelity of a file on disk is not guaranteed — a
   * non-starter for editing tracked repo files), TinyMCE (HTML-first,
   * licence-encumbered), and a plain `<textarea>`. CodeMirror 6 wins on three
   * axes that matter here: it edits the SOURCE TEXT, so what is saved is
   * exactly what was typed; it ships as tree-shakeable ES modules with no CDN
   * fetch, which the Tauri build and #4401's CSP constraint both require; and
   * its extension/decoration model is the mechanism #4401 needs to render
   * ```mermaid blocks as live diagrams *inside* the document. A textarea would
   * have to be thrown away to get there.
   *
   * What: a controlled editing surface. `value` in, `change` out, `save` out —
   * no fetching, no persistence, no knowledge of projects or routes, which is
   * what makes it reusable per the issue's acceptance. `readonly` is the seam
   * #4360 drives: it flips the same mounted editor to view-only for
   * non-markdown documents without a second component.
   *
   * DELIBERATELY NOT HERE: rendered preview and Mermaid diagrams. #4401 owns
   * the canvas layer and lists the render path and the edit/preview UX as open
   * owner questions — shipping either here would answer them by fiat. Tab is
   * likewise left as focus-movement rather than bound to indent, so the editor
   * stays keyboard-escapable (CodeMirror's documented accessibility default).
   *
   * Test: `MarkdownEditor.test.ts`.
   */
  import { createEventDispatcher, onDestroy, onMount } from 'svelte';
  import { Compartment, EditorState, type Extension } from '@codemirror/state';
  import {
    EditorView,
    highlightActiveLine,
    highlightActiveLineGutter,
    keymap,
    lineNumbers,
    placeholder as placeholderExtension,
  } from '@codemirror/view';
  import { defaultKeymap, history, historyKeymap } from '@codemirror/commands';
  import { markdown, markdownLanguage } from '@codemirror/lang-markdown';
  import { HighlightStyle, syntaxHighlighting } from '@codemirror/language';
  import { tags } from '@lezer/highlight';

  /** Markdown source. Controlled: the parent owns it and re-feeds every edit. */
  export let value = '';
  /** View-only when true — the #4360 document-type gate's lever. */
  export let readonly = false;
  /** Shown while the document is empty. */
  export let placeholder = '';
  /** Accessible name for the editing region. */
  export let ariaLabel = 'Markdown editor';

  const dispatch = createEventDispatcher<{
    change: { value: string };
    save: { value: string };
  }>();

  let host: HTMLDivElement;
  let view: EditorView | null = null;

  /**
   * Editability lives in a compartment so a `readonly` flip reconfigures the
   * live editor in place. Recreating the view instead would drop undo history
   * and the cursor, which is exactly what #4360 must not do when it toggles
   * documents in a shared pane.
   */
  const editability = new Compartment();

  function editabilityFor(ro: boolean): Extension {
    return [EditorView.editable.of(!ro), EditorState.readOnly.of(ro)];
  }

  /**
   * Class-based highlighting rather than inline colours: the app's palette is
   * CSS custom properties that flip on `.dark` (`app.css`), so styling by class
   * makes the editor follow the theme with no reconfiguration on toggle.
   */
  const markdownHighlight = HighlightStyle.define([
    { tag: tags.heading, class: 'cm-md-heading' },
    { tag: tags.strong, class: 'cm-md-strong' },
    { tag: tags.emphasis, class: 'cm-md-emphasis' },
    { tag: [tags.link, tags.url], class: 'cm-md-link' },
    { tag: tags.monospace, class: 'cm-md-code' },
    { tag: tags.quote, class: 'cm-md-quote' },
    { tag: tags.list, class: 'cm-md-marker' },
    { tag: tags.processingInstruction, class: 'cm-md-marker' },
    { tag: tags.strikethrough, class: 'cm-md-strike' },
  ]);

  onMount(() => {
    view = new EditorView({
      parent: host,
      state: EditorState.create({
        doc: value,
        extensions: [
          lineNumbers(),
          highlightActiveLine(),
          highlightActiveLineGutter(),
          history(),
          EditorView.lineWrapping,
          markdown({ base: markdownLanguage }),
          syntaxHighlighting(markdownHighlight),
          placeholderExtension(placeholder),
          keymap.of([
            {
              key: 'Mod-s',
              preventDefault: true,
              run: (v) => {
                dispatch('save', { value: v.state.doc.toString() });
                return true;
              },
            },
            ...historyKeymap,
            ...defaultKeymap,
          ]),
          editability.of(editabilityFor(readonly)),
          EditorView.updateListener.of((update) => {
            if (!update.docChanged) return;
            dispatch('change', { value: update.state.doc.toString() });
          }),
          EditorView.contentAttributes.of({ 'aria-label': ariaLabel }),
        ],
      }),
    });
  });

  onDestroy(() => {
    view?.destroy();
    view = null;
  });

  /**
   * Push an externally-changed `value` into the document.
   *
   * The equality check is load-bearing, not an optimisation: without it the
   * parent echoing our own `change` back would replace the whole document on
   * every keystroke, resetting the selection to the start of the line.
   */
  function syncFromProp(next: string) {
    if (!view || view.state.doc.toString() === next) return;
    view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: next } });
  }

  $: syncFromProp(value);
  $: view?.dispatch({ effects: editability.reconfigure(editabilityFor(readonly)) });
</script>

<div
  bind:this={host}
  class="markdown-editor h-full min-h-0 overflow-hidden rounded-md border border-foundry-light-border dark:border-foundry-border bg-foundry-light-bg dark:bg-foundry-bg font-mono"
  class:is-readonly={readonly}
></div>

<style>
  /* CodeMirror renders its own DOM inside the host, so every rule below has to
     be :global. Colours come from app.css's --color-* tokens (Foundry v2), which
     already invert under .dark — no per-theme CodeMirror config needed. */
  .markdown-editor :global(.cm-editor) {
    height: 100%;
    background: transparent;
    color: rgb(var(--color-text-primary));
    font-family: inherit;
    font-size: 0.8125rem;
    line-height: 1.6;
  }
  .markdown-editor :global(.cm-editor.cm-focused) {
    outline: 2px solid rgb(var(--color-primary) / 0.6);
    outline-offset: -2px;
  }
  .markdown-editor :global(.cm-scroller) {
    font-family: inherit;
    overflow: auto;
  }
  .markdown-editor :global(.cm-gutters) {
    background: transparent;
    border-right: 1px solid rgb(var(--color-border));
    color: rgb(var(--color-text-muted) / 0.7);
  }
  .markdown-editor :global(.cm-activeLine),
  .markdown-editor :global(.cm-activeLineGutter) {
    background: rgb(var(--color-primary) / 0.06);
  }
  .markdown-editor.is-readonly :global(.cm-activeLine),
  .markdown-editor.is-readonly :global(.cm-activeLineGutter) {
    background: transparent;
  }
  .markdown-editor :global(.cm-selectionBackground),
  .markdown-editor :global(.cm-content ::selection) {
    background: rgb(var(--color-primary) / 0.25);
  }
  .markdown-editor :global(.cm-cursor) {
    border-left-color: rgb(var(--color-primary));
  }
  .markdown-editor :global(.cm-placeholder) {
    color: rgb(var(--color-text-muted) / 0.7);
  }

  /* Markdown token styling — see markdownHighlight above. */
  .markdown-editor :global(.cm-md-heading) {
    color: rgb(var(--color-primary));
    font-weight: 600;
  }
  .markdown-editor :global(.cm-md-strong) {
    font-weight: 700;
  }
  .markdown-editor :global(.cm-md-emphasis) {
    font-style: italic;
  }
  .markdown-editor :global(.cm-md-link) {
    color: rgb(var(--color-info));
    text-decoration: underline;
  }
  .markdown-editor :global(.cm-md-code) {
    color: rgb(var(--color-success));
  }
  .markdown-editor :global(.cm-md-quote) {
    color: rgb(var(--color-text-muted));
    font-style: italic;
  }
  .markdown-editor :global(.cm-md-marker) {
    color: rgb(var(--color-text-muted));
  }
  .markdown-editor :global(.cm-md-strike) {
    text-decoration: line-through;
  }
</style>
