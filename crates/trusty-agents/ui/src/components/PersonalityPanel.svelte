<script lang="ts">
  /**
   * Why: #3061 introduced this panel as a minimal prose editor bound
   * directly to a SINGLE hardcoded personalization overlay
   * (`~/.trusty-agents/agents/my-assistant.md`, DOC-41 §2.5.1's `extends:`
   * mechanism). #3224 (Trusty Agents agent roster, epic #3052)
   * generalizes it to edit ANY overlay the user has created, plus a
   * "+ New agent" flow (name + pasted instructions + optional display name
   * + base to extend) — the roster's natural host for customizing an agent,
   * per the issue. Still NOT a structured personality schema — prose only,
   * per #3061's original scope. Thin wrapper around the
   * `read_personalization_overlay` / `write_personalization_overlay` /
   * `list_personalization_overlays` / `delete_personalization_overlay` Tauri
   * commands (`ui/src-tauri/src/overlay.rs`).
   * What: A target picker (existing overlays + "+ New agent") drives which
   * `.md` file the textarea below edits. Loads the target's current content
   * via `invoke('read_personalization_overlay', ...)`; a not-found result
   * (only possible for the legacy default target before its first save)
   * pre-fills a starter template instead of an error. The "+ New agent"
   * form collects a display name (live-slugified into the filename), an
   * optional base to extend (defaults to the base `assistant`, offers any
   * other base/catalog roster entry — e.g. `cto-assistant` — when present),
   * and pasted instructions, then writes a fresh overlay and switches to
   * editing it. Delete removes the on-disk file after a native confirm().
   * Save/Create/Delete all call `refreshOverlayAgents()` afterward so the
   * roster switcher reflects the change immediately. Unsaved-changes
   * tracking (bound to `App.svelte`'s tab-switch guard) covers both the
   * prose editor buffer AND an in-progress "+ New agent" form. In browser
   * mode (no Tauri IPC) renders a "desktop app only" empty state, since
   * there is no REST equivalent for direct filesystem access.
   * Test: Manual — open the tab with no overlays, confirm the "+ New agent"
   * form is the default view; create one, confirm it appears in the picker
   * AND the roster switcher; edit + Save, confirm the unsaved-changes dot
   * clears; Delete, confirm a confirm() dialog gates it and the file is
   * gone from both the picker and the roster.
   */
  import { onMount } from 'svelte';
  import { User, Save, AlertCircle, FileText, Trash2, Plus } from 'lucide-svelte';
  import { invoke, isDesktop } from '../lib/transport';
  import { agentRoster, overlayAgents, refreshOverlayAgents } from '../stores/app';
  import { buildOverlayMarkdown, slugify } from '../lib/roster';

  // Why: The legacy #3061 default — kept as the fallback target so an
  // existing user's overlay still loads by default on first open after this
  // generalization, rather than dropping them into an empty "+ New agent"
  // form.
  const LEGACY_DEFAULT_SLUG = 'my-assistant';

  const desktop = isDesktop();

  /** Which overlay slug the editor below is bound to; `null` = "+ New agent" form is showing. */
  let targetSlug: string | null = null;
  let pickerInitialized = false;

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
  let deleting = false;

  // "+ New agent" form state (#3224).
  let newDisplayName = '';
  let newExtends = 'assistant';
  let newInstructions = '';
  let creating = false;
  let createError = '';

  $: newSlugPreview = slugify(newDisplayName);

  // Why: Any base/catalog roster entry is a valid `extends:` target — a
  // user overlay can personalize the plain assistant OR a specific catalog
  // persona like `cto-assistant`. Overlay entries are excluded to keep the
  // extends chain shallow (overlay-extends-overlay is untested territory).
  $: extendsOptions = $agentRoster.filter((entry) => entry.source !== 'overlay');

  // Why (#3198 code-critic HIGH, generalized for #3224): exported so
  // App.svelte can `bind:unsavedChanges` and guard tab switches. Now covers
  // BOTH the prose editor buffer and an in-progress "+ New agent" form —
  // either one losing state on an unguarded tab switch would be the same
  // silent-discard bug the original guard was written to prevent.
  export let unsavedChanges = false;
  $: unsavedChanges =
    targetSlug === null
      ? newDisplayName.trim() !== '' || newInstructions.trim() !== ''
      : content !== savedContent;

  interface OverlayResult {
    content: string | null;
    path: string;
  }

  function starterTemplate(slug: string): string {
    return `---
name: ${slug}
extends: assistant
display_name: ${slug}
---

Replace this paragraph with your own instructions: describe how you want
your assistant to talk to you, what it should prioritize, and any personal
context it should always remember. This text is appended after the base
"assistant" agent's prompt, so write it in your own voice.
`;
  }

  async function load(slug: string) {
    loading = true;
    loadError = '';
    try {
      const result = await invoke<OverlayResult>('read_personalization_overlay', {
        name: slug,
      });
      overlayPath = result.path;
      if (result.content === null) {
        notFound = true;
        content = starterTemplate(slug);
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
    if (!targetSlug) return;
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
      await invoke('write_personalization_overlay', { name: targetSlug, content: toSave });
      savedContent = toSave;
      notFound = false;
      justSaved = true;
      if (justSavedTimer) clearTimeout(justSavedTimer);
      justSavedTimer = setTimeout(() => (justSaved = false), 2000);
      await refreshOverlayAgents();
    } catch (e) {
      saveError = (e as Error).message ?? String(e);
    } finally {
      saving = false;
    }
  }

  /**
   * Why: Deleting is destructive and unrecoverable, so it gates on a native
   * `confirm()` the same way Sidebar's clear-context flow does. Trivial to
   * implement (a thin wrapper over `delete_personalization_overlay`) per
   * the issue's "only if trivial" scope.
   * What: Confirms, deletes the on-disk file, refreshes the roster, and
   * falls back to the "+ New agent" form (or another remaining overlay).
   */
  async function handleDelete() {
    if (!targetSlug) return;
    if (!confirm(`Delete the "${targetSlug}" personalization overlay? This cannot be undone.`)) {
      return;
    }
    deleting = true;
    try {
      await invoke('delete_personalization_overlay', { name: targetSlug });
      await refreshOverlayAgents();
      const remaining = $overlayAgents.filter((o) => o.slug !== targetSlug);
      targetSlug = remaining.length > 0 ? remaining[0].slug : null;
      if (targetSlug) {
        await load(targetSlug);
      }
    } catch (e) {
      loadError = (e as Error).message ?? String(e);
    } finally {
      deleting = false;
    }
  }

  async function selectTarget(slug: string | null) {
    targetSlug = slug;
    if (slug) {
      await load(slug);
    }
  }

  /**
   * Why: The roster's create-agent affordance (#3224) — a name + pasted
   * instructions is enough to produce a valid overlay; display name and
   * base-to-extend are optional conveniences with sane defaults.
   * What: Slugifies `newDisplayName`, builds a frontmatter + prose overlay
   * body, writes it via the same `write_personalization_overlay` command
   * Save uses, then switches the editor to the newly-created overlay.
   */
  async function handleCreate() {
    createError = '';
    const slug = newSlugPreview;
    if (!slug) {
      createError = 'Enter a name — it needs at least one letter, digit, "_", or "-".';
      return;
    }
    if (!newInstructions.trim()) {
      createError = 'Paste some instructions for the agent.';
      return;
    }
    creating = true;
    try {
      // Why: `buildOverlayMarkdown` (lib/roster.ts) is the single place that
      // assembles the frontmatter+prose shape both this form and the Rust
      // loader agree on — reusing it here instead of hand-building the
      // string keeps Save/Create from silently drifting apart.
      const body = buildOverlayMarkdown(slug, {
        displayName: newDisplayName.trim() || slug,
        extends: newExtends,
        instructions: newInstructions,
      });
      await invoke('write_personalization_overlay', { name: slug, content: body });
      await refreshOverlayAgents();
      newDisplayName = '';
      newInstructions = '';
      newExtends = 'assistant';
      await selectTarget(slug);
    } catch (e) {
      createError = (e as Error).message ?? String(e);
    } finally {
      creating = false;
    }
  }

  onMount(() => {
    if (desktop) {
      (async () => {
        await refreshOverlayAgents();
        pickerInitialized = true;
        const overlays = $overlayAgents;
        const legacy = overlays.find((o) => o.slug === LEGACY_DEFAULT_SLUG);
        const initial = legacy?.slug ?? overlays[0]?.slug ?? null;
        await selectTarget(initial);
        // A brand-new install with zero overlays: seed the legacy default so
        // existing muscle-memory ("Personality" tab → my assistant) still
        // works, rather than forcing every user into the create form.
        if (initial === null) {
          targetSlug = LEGACY_DEFAULT_SLUG;
          await load(LEGACY_DEFAULT_SLUG);
        }
      })();
    } else {
      loading = false;
    }
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
    {#if desktop && pickerInitialized}
      <div class="flex items-center gap-2">
        <select
          class="rounded-md border border-foundry-light-border dark:border-foundry-border bg-foundry-light-surface dark:bg-foundry-surface px-2 py-1 text-xs text-foundry-light-text dark:text-foundry-text"
          value={targetSlug ?? '__new__'}
          on:change={(e) => {
            const v = (e.currentTarget as HTMLSelectElement).value;
            selectTarget(v === '__new__' ? null : v);
          }}
        >
          {#each $overlayAgents as overlay (overlay.slug)}
            <option value={overlay.slug}>{overlay.display_name || overlay.name || overlay.slug}</option>
          {/each}
          <option value="__new__">+ New agent…</option>
        </select>
        {#if targetSlug && !loading && !loadError}
          <button
            type="button"
            on:click={save}
            disabled={saving || !unsavedChanges}
            class="inline-flex items-center gap-1 border border-foundry-light-primary dark:border-foundry-primary bg-foundry-light-primary dark:bg-foundry-primary px-3 py-1 text-xs font-medium text-white hover:bg-foundry-light-primary/80 dark:hover:bg-foundry-primary/80 disabled:cursor-not-allowed disabled:border-foundry-light-border dark:disabled:border-foundry-border disabled:bg-foundry-light-surface dark:disabled:bg-foundry-surface disabled:text-foundry-light-muted dark:disabled:text-foundry-text/40"
          >
            <Save class="h-3 w-3" />
            {saving ? 'Saving…' : justSaved ? 'Saved' : 'Save'}
          </button>
          <button
            type="button"
            on:click={handleDelete}
            disabled={deleting}
            class="inline-flex items-center gap-1 border border-red-500/40 px-3 py-1 text-xs font-medium text-red-600 dark:text-red-400 hover:bg-red-500/10 disabled:cursor-not-allowed disabled:opacity-50"
          >
            <Trash2 class="h-3 w-3" />
            {deleting ? 'Deleting…' : 'Delete'}
          </button>
        {/if}
      </div>
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
          Editing a personalization overlay reads and writes files under
          <code class="font-mono">~/.trusty-agents/agents/</code> directly, which
          is only available in the Trusty Agents desktop app — not this
          browser preview.
        </p>
      </div>
    {:else if targetSlug === null}
      <!-- #3224: "+ New agent" — name + pasted instructions is enough. -->
      <div class="mx-auto flex max-w-lg flex-col gap-3">
        <div class="flex items-center gap-2 text-foundry-light-text dark:text-foundry-text">
          <Plus class="h-4 w-4 text-foundry-light-primary dark:text-foundry-primary" />
          <h2 class="text-sm font-semibold">New agent</h2>
        </div>
        <label class="flex flex-col gap-1 text-xs text-foundry-light-muted dark:text-foundry-text/70">
          Display name
          <input
            type="text"
            bind:value={newDisplayName}
            placeholder="My CTO"
            class="rounded-md border border-foundry-light-border dark:border-foundry-border bg-foundry-light-surface dark:bg-foundry-surface px-3 py-1.5 text-sm text-foundry-light-text dark:text-foundry-text focus:border-foundry-light-primary dark:focus:border-foundry-primary focus:outline-none"
          />
          {#if newSlugPreview}
            <span class="font-mono text-[10px] text-foundry-light-muted dark:text-foundry-text/40">
              will save as ~/.trusty-agents/agents/{newSlugPreview}.md
            </span>
          {/if}
        </label>
        <label class="flex flex-col gap-1 text-xs text-foundry-light-muted dark:text-foundry-text/70">
          Extends (base persona)
          <select
            bind:value={newExtends}
            class="rounded-md border border-foundry-light-border dark:border-foundry-border bg-foundry-light-surface dark:bg-foundry-surface px-2 py-1.5 text-sm text-foundry-light-text dark:text-foundry-text"
          >
            {#each extendsOptions as entry (entry.id)}
              <option value={entry.id}>{entry.label}</option>
            {/each}
          </select>
        </label>
        <label class="flex flex-col gap-1 text-xs text-foundry-light-muted dark:text-foundry-text/70">
          Instructions
          <textarea
            bind:value={newInstructions}
            rows="8"
            spellcheck="false"
            placeholder="Paste or write how you want this agent to talk to you, what it should prioritize, and any context it should always remember."
            class="resize-none rounded-md border border-foundry-light-border dark:border-foundry-border bg-foundry-light-surface dark:bg-foundry-surface px-3 py-2 font-mono text-sm leading-relaxed text-foundry-light-text dark:text-foundry-text focus:border-foundry-light-primary dark:focus:border-foundry-primary focus:outline-none"
          ></textarea>
        </label>
        {#if createError}
          <div class="border border-red-500/40 bg-red-500/5 px-3 py-2 text-xs text-red-600 dark:text-red-400">
            {createError}
          </div>
        {/if}
        <button
          type="button"
          on:click={handleCreate}
          disabled={creating}
          class="inline-flex items-center justify-center gap-1 rounded-md border border-foundry-light-primary dark:border-foundry-primary bg-foundry-light-primary dark:bg-foundry-primary px-3 py-2 text-sm font-medium text-white hover:bg-foundry-light-primary/80 dark:hover:bg-foundry-primary/80 disabled:cursor-not-allowed disabled:opacity-60"
        >
          <Plus class="h-3.5 w-3.5" />
          {creating ? 'Creating…' : 'Create agent'}
        </button>
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
            Save to create <code class="font-mono">{overlayPath || `~/.trusty-agents/agents/${targetSlug}.md`}</code>.
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
        Edits apply to <code class="font-mono">{overlayPath || `~/.trusty-agents/agents/${targetSlug}.md`}</code>.
        Prose only — this is not a structured settings form.
      </p>
    {/if}
  </div>
</section>
