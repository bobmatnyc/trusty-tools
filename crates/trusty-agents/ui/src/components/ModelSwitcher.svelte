<script lang="ts">
  /**
   * Why (#3245 — model/provider picker, epic #3052; depends on #3243's
   * `GET /api/models` catalog): the Assistant MVP goal "choose which AI
   * model/provider the Assistant uses" (e.g. switch between Claude, local
   * Ollama, different Claude sizes) needs a visible picker, mirroring
   * `AgentSwitcher.svelte`'s roster dropdown pattern exactly — same compact
   * button + listbox shape, same store-on-select wiring, same graceful
   * empty/loading/error handling (an unreachable `/api/models` leaves the
   * picker showing only "Default" rather than crashing the input area).
   * What: Renders the active picker entry's label as a rectangular button;
   * clicking opens a list of every `buildPicker($modelCatalog)` row.
   * Unselectable rows (no credential configured, or not yet wired for
   * dispatch, or Ollama unreachable) render disabled with a reason. Selecting
   * a row sets `activeModelEntry` (read by `InputArea.svelte` on next
   * submit) and closes the list. Fetches `/api/models` on mount.
   * Test: Manual — open the app, confirm "Default" renders by default;
   * click it, confirm the list opens with every catalog provider + Ollama;
   * click a credentialed row, confirm the button label updates and the next
   * chat message's `POST /api/task` body carries `model_id`/`provider_id`.
   */
  import { onMount } from 'svelte';
  import { ChevronDown, Cpu } from 'lucide-svelte';
  import { activeModelEntry, fetchModelCatalog, modelCatalog } from '../stores/app';
  import { DEFAULT_PICKER_ID, buildPicker, type PickerEntry } from '../lib/models';

  let open = false;
  let loadError = false;
  let picker: PickerEntry[] = [];

  $: picker = $modelCatalog ? buildPicker($modelCatalog) : [];
  $: activeEntry = $activeModelEntry ?? picker[0] ?? null;

  function toggle() {
    open = !open;
  }

  function select(entry: PickerEntry) {
    if (!entry.selectable) return;
    activeModelEntry.set(entry.id === DEFAULT_PICKER_ID ? null : entry);
    open = false;
  }

  function handleWindowClick(event: MouseEvent) {
    if (!open) return;
    const target = event.target as HTMLElement;
    if (!target.closest('[data-model-switcher]')) {
      open = false;
    }
  }

  onMount(() => {
    fetchModelCatalog().catch((e) => {
      console.error('[ModelSwitcher] fetchModelCatalog failed:', e);
      loadError = true;
    });
  });
</script>

<svelte:window on:click={handleWindowClick} />

<div class="relative inline-block" data-model-switcher>
  <button
    type="button"
    class="flex items-center gap-1.5 rounded-md border border-foundry-light-border dark:border-foundry-border bg-foundry-light-surface dark:bg-foundry-surface px-2.5 py-1 font-mono text-[11px] font-semibold uppercase tracking-wide text-foundry-light-text dark:text-foundry-text hover:border-foundry-light-primary dark:hover:border-foundry-primary"
    on:click={toggle}
    aria-haspopup="listbox"
    aria-expanded={open}
    title={loadError ? 'Model catalog unavailable — using default' : undefined}
  >
    <Cpu class="h-3.5 w-3.5 text-foundry-light-primary dark:text-foundry-primary" />
    {activeEntry?.label ?? 'Default'}
    <ChevronDown class="h-3 w-3 opacity-60" />
  </button>

  {#if open}
    <ul
      role="listbox"
      class="absolute right-0 top-full z-30 mt-1 max-h-72 w-64 overflow-y-auto rounded-md border border-foundry-light-border dark:border-foundry-border bg-foundry-light-surface dark:bg-foundry-surface py-1 shadow-lg"
    >
      {#if picker.length === 0}
        <li class="px-3 py-1.5 text-xs text-foundry-light-muted dark:text-foundry-text/40">
          {loadError ? 'Model catalog unavailable' : 'Loading…'}
        </li>
      {/if}
      {#each picker as entry (entry.id)}
        <li>
          <button
            type="button"
            role="option"
            aria-selected={entry.id === (activeEntry?.id ?? DEFAULT_PICKER_ID)}
            disabled={!entry.selectable}
            class="flex w-full flex-col items-start gap-0.5 px-3 py-1.5 text-left text-xs transition-colors disabled:cursor-not-allowed disabled:opacity-40 {entry.id ===
            (activeEntry?.id ?? DEFAULT_PICKER_ID)
              ? 'bg-foundry-light-primary/15 dark:bg-foundry-primary/15 text-foundry-light-primary dark:text-foundry-primary'
              : 'text-foundry-light-text/80 dark:text-foundry-text/80 hover:bg-foundry-light-primary/10 dark:hover:bg-foundry-primary/10'}"
            on:click={() => select(entry)}
          >
            <span class="font-medium">{entry.label}</span>
            {#if !entry.selectable}
              <span class="font-mono text-[10px] uppercase tracking-wide text-foundry-light-muted dark:text-foundry-text/40">
                unavailable
              </span>
            {/if}
          </button>
        </li>
      {/each}
    </ul>
  {/if}
</div>
