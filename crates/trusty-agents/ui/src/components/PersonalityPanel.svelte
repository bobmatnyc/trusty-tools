<script lang="ts">
  /**
   * Why: #3061 — users have no way to customize how their assistant talks to
   * them short of hand-editing `~/.trusty-agents/agents/my-assistant.md` in a
   * terminal. This panel is a minimal prose editor bound directly to that
   * personalization overlay (DOC-41 §2.5.1's `extends:` mechanism) — NOT a
   * structured personality schema, per the issue's scope. It is a thin
   * wrapper around the `read_personalization_overlay` /
   * `write_personalization_overlay` Tauri commands (ui/src-tauri/src/main.rs)
   * added alongside this panel.
   * What: On mount (desktop only), loads `my-assistant.md`'s current content
   * via `invoke('read_personalization_overlay', ...)`. A not-found result
   * pre-fills the textarea with a starter template (frontmatter +
   * explanatory prose) instead of an error. Save writes the buffer back
   * verbatim. Tracks unsaved changes by diffing the buffer against the last
   * loaded/saved snapshot. In browser mode (no Tauri IPC — see
   * `isDesktop()` in `lib/transport.ts`) renders a "desktop app only" empty
   * state instead of a non-functional editor, since there is no REST
   * equivalent for direct filesystem access.
   * Test: Manual — in the Tauri app, open the Personality tab with no
   * overlay file present, confirm the starter template + not-found notice
   * render; type, confirm the unsaved-changes dot appears; Save, confirm it
   * clears and the file exists on disk with matching content; reload the
   * tab, confirm the saved content (not the template) loads. In `pnpm dev`
   * (browser), confirm the "desktop app only" empty state renders instead.
   */
  import { onMount } from 'svelte';
  import { User, Save, AlertCircle, FileText } from 'lucide-svelte';
  import { invoke, isDesktop } from '../lib/transport';

  // Why: Fixed target for v1 — the issue scopes this to a single overlay
  // (`my-assistant.md`, extending the nameless "assistant" base agent) rather
  // than a picker over every possible agent name. A multi-agent overlay
  // picker is a natural follow-up, not required here.
  const AGENT_NAME = 'my-assistant';

  const STARTER_TEMPLATE = `---
name: my-assistant
extends: assistant
display_name: My Assistant
---

Replace this paragraph with your own instructions: describe how you want
your assistant to talk to you, what it should prioritize, and any personal
context it should always remember. This text is appended after the base
"assistant" agent's prompt, so write it in your own voice.
`;

  const desktop = isDesktop();

  let loading = true;
  let loadError = '';
  let notFound = false;
  let overlayPath = '';
  let content = '';
  let savedContent = '';
  let saving = false;
  let saveError = '';
  let justSaved = false;
  let justSavedTimer: ReturnType<typeof setTimeout> | null = null;

  // Why (#3198 code-critic HIGH): exported (not just local) so App.svelte can
  // `bind:unsavedChanges` and guard tab switches — the {#if}/{:else if} tab
  // router unmounts this component on navigation, which would otherwise
  // silently discard an unsaved buffer despite the "Unsaved" badge shown
  // below.
  export let unsavedChanges = false;
  $: unsavedChanges = content !== savedContent;

  interface OverlayResult {
    content: string | null;
    path: string;
  }

  async function load() {
    loading = true;
    loadError = '';
    try {
      const result = await invoke<OverlayResult>('read_personalization_overlay', {
        name: AGENT_NAME,
      });
      overlayPath = result.path;
      if (result.content === null) {
        notFound = true;
        content = STARTER_TEMPLATE;
        // Why: an unsaved starter template should show the unsaved-changes
        // indicator (nothing has been persisted yet), so savedContent stays
        // empty rather than mirroring the template.
        savedContent = '';
      } else {
        notFound = false;
        content = result.content;
        savedContent = result.content;
      }
    } catch (e) {
      loadError = (e as Error).message ?? String(e);
    } finally {
      loading = false;
    }
  }

  async function save() {
    saving = true;
    saveError = '';
    // Why (#3198 code-critic MEDIUM): snapshot the buffer BEFORE the await.
    // If the user keeps typing during the IPC round-trip, `content` can
    // change while `write_personalization_overlay` is in flight; comparing
    // against a snapshot (and marking only that snapshot saved) means
    // keystrokes typed mid-save correctly stay flagged as unsaved instead of
    // being silently treated as persisted.
    const toSave = content;
    try {
      await invoke('write_personalization_overlay', { name: AGENT_NAME, content: toSave });
      savedContent = toSave;
      notFound = false;
      justSaved = true;
      if (justSavedTimer) clearTimeout(justSavedTimer);
      justSavedTimer = setTimeout(() => (justSaved = false), 2000);
    } catch (e) {
      saveError = (e as Error).message ?? String(e);
    } finally {
      saving = false;
    }
  }

  onMount(() => {
    if (desktop) load();
    return () => {
      if (justSavedTimer) clearTimeout(justSavedTimer);
    };
  });
</script>

<section class="flex h-full flex-1 flex-col overflow-hidden bg-foundry-light-bg dark:bg-foundry-bg">
  <header
    class="flex flex-wrap items-center justify-between gap-3 border-b border-foundry-light-border dark:border-foundry-border px-6 py-3"
  >
    <div class="flex items-center gap-2">
      <User class="h-4 w-4 text-foundry-light-primary dark:text-foundry-primary" />
      <h1 class="text-lg font-semibold text-foundry-light-text dark:text-foundry-text">Personality</h1>
      {#if desktop && unsavedChanges}
        <span
          class="inline-flex items-center gap-1 border border-foundry-amber/40 bg-foundry-amber/10 px-2 py-0.5 text-xs text-foundry-amber"
          title="Unsaved changes"
        >
          <span class="inline-block h-1.5 w-1.5 bg-foundry-amber" aria-hidden="true"></span>
          Unsaved
        </span>
      {/if}
    </div>
    {#if desktop && !loading && !loadError}
      <button
        type="button"
        on:click={save}
        disabled={saving || !unsavedChanges}
        class="inline-flex items-center gap-1 border border-foundry-light-primary dark:border-foundry-primary bg-foundry-light-primary dark:bg-foundry-primary px-3 py-1 text-xs font-medium text-white hover:bg-foundry-light-primary/80 dark:hover:bg-foundry-primary/80 disabled:cursor-not-allowed disabled:border-foundry-light-border dark:disabled:border-foundry-border disabled:bg-foundry-light-surface dark:disabled:bg-foundry-surface disabled:text-foundry-light-muted dark:disabled:text-foundry-text/40"
      >
        <Save class="h-3 w-3" />
        {saving ? 'Saving…' : justSaved ? 'Saved' : 'Save'}
      </button>
    {/if}
  </header>

  <div class="flex-1 overflow-y-auto px-6 py-4">
    {#if !desktop}
      <!-- Tauri-only: no REST equivalent for direct filesystem access to
           ~/.trusty-agents/agents/, so the browser build gets a clear empty
           state rather than a broken editor. -->
      <div
        class="flex flex-col items-center justify-center gap-3 border border-dashed border-foundry-light-border dark:border-foundry-border px-6 py-12 text-center"
      >
        <FileText class="h-6 w-6 text-foundry-light-muted dark:text-foundry-text/40" />
        <p class="text-sm font-medium text-foundry-light-text dark:text-foundry-text">Desktop app only</p>
        <p class="max-w-sm text-xs text-foundry-light-muted dark:text-foundry-text/60">
          Editing your personalization overlay reads and writes files under
          <code class="font-mono">~/.trusty-agents/agents/</code> directly, which
          is only available in the Trusty Assistant desktop app — not this
          browser preview.
        </p>
      </div>
    {:else if loading}
      <p class="text-sm text-foundry-light-muted dark:text-foundry-text/60">Loading…</p>
    {:else if loadError}
      <div class="border border-red-500/40 bg-red-500/5 px-4 py-3 text-sm text-red-600 dark:text-red-400">
        {loadError}
      </div>
    {:else}
      {#if notFound}
        <div
          class="mb-3 flex items-start gap-2 border border-foundry-light-primary/30 dark:border-foundry-primary/30 bg-foundry-light-primary/5 dark:bg-foundry-primary/5 px-4 py-3 text-xs text-foundry-light-text/80 dark:text-foundry-text/80"
        >
          <AlertCircle class="mt-0.5 h-3.5 w-3.5 shrink-0 text-foundry-light-primary dark:text-foundry-primary" />
          <span>
            No overlay found yet — a starter template is pre-filled below. Edit it and click
            Save to create <code class="font-mono">{overlayPath || `~/.trusty-agents/agents/${AGENT_NAME}.md`}</code>.
          </span>
        </div>
      {/if}
      {#if saveError}
        <div class="mb-3 border border-red-500/40 bg-red-500/5 px-4 py-2 text-xs text-red-600 dark:text-red-400">
          {saveError}
        </div>
      {/if}
      <!--
        Prose-only editor (no structured schema, per #3061 scope). IBM Plex
        Mono via font-mono gives the frontmatter block a config-file feel
        while the free-prose body underneath stays readable.
      -->
      <textarea
        bind:value={content}
        spellcheck="false"
        autocomplete="off"
        class="h-full min-h-[24rem] w-full resize-none border border-foundry-light-border dark:border-foundry-border bg-foundry-light-surface dark:bg-foundry-surface px-4 py-3 font-mono text-sm leading-relaxed text-foundry-light-text dark:text-foundry-text focus:border-foundry-light-primary dark:focus:border-foundry-primary focus:outline-none"
        aria-label="Personalization overlay editor"
      ></textarea>
      <p class="mt-2 text-xs text-foundry-light-muted dark:text-foundry-text/50">
        Edits apply to <code class="font-mono">{overlayPath || `~/.trusty-agents/agents/${AGENT_NAME}.md`}</code>.
        Prose only — this is not a structured settings form.
      </p>
    {/if}
  </div>
</section>
