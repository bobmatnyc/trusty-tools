<script lang="ts">
  /**
   * Why (#3819): the add-agent flow, per Bob's spec correction — trusty-
   * agents has no sub-agents, only agent TEMPLATES, and this slice ships
   * exactly one: `assistant`. Kept deliberately minimal (name + optional
   * description, no template picker since there's only one choice) —
   * flagged in the PR body as the "stub if time is short" candidate, but
   * wired to a real `POST /api/agents` rather than a disabled button.
   * What: A tiny inline form (not a modal — lives inside `ChatHeader`'s
   * dropdown) that slugifies the display name live and posts on submit.
   * Test: Manual — open the agent switcher, click "+ Add agent", type a
   * name, submit, confirm the new agent appears in the roster and becomes
   * active.
   */
  import { createEventDispatcher } from 'svelte';
  import { apiBase } from '../lib/api-config';
  import { getCurrentApiToken } from '../stores/app';
  import { slugify } from '../lib/roster';

  const dispatch = createEventDispatcher<{ created: { name: string }; cancel: void }>();

  let displayName = '';
  let description = '';
  let submitting = false;
  let error = '';

  $: slugPreview = slugify(displayName);

  async function submit() {
    const name = slugPreview;
    if (!name) {
      error = 'Enter a name.';
      return;
    }
    submitting = true;
    error = '';
    try {
      const token = getCurrentApiToken();
      const headers: Record<string, string> = { 'Content-Type': 'application/json' };
      if (token) headers.Authorization = `Bearer ${token}`;
      const r = await fetch(`${apiBase()}/api/agents`, {
        method: 'POST',
        headers,
        body: JSON.stringify({
          name,
          description: description.trim() || undefined,
          template: 'assistant',
        }),
      });
      if (!r.ok) {
        const body = (await r.json().catch(() => ({}))) as { error?: string };
        throw new Error(body.error ?? `POST /api/agents failed: ${r.status}`);
      }
      dispatch('created', { name });
    } catch (e) {
      error = `${e}`;
    } finally {
      submitting = false;
    }
  }
</script>

<form on:submit|preventDefault={submit} class="flex flex-col gap-1.5 pt-1">
  <input
    type="text"
    bind:value={displayName}
    placeholder="New agent name"
    autocomplete="off"
    class="rounded-md border border-foundry-light-border dark:border-foundry-primary/30 bg-foundry-light-bg dark:bg-foundry-bg px-2 py-1 text-xs text-foundry-light-text dark:text-foundry-text focus:border-foundry-light-primary dark:focus:border-foundry-primary focus:outline-none"
  />
  <input
    type="text"
    bind:value={description}
    placeholder="Description (optional)"
    autocomplete="off"
    class="rounded-md border border-foundry-light-border dark:border-foundry-primary/30 bg-foundry-light-bg dark:bg-foundry-bg px-2 py-1 text-xs text-foundry-light-text dark:text-foundry-text focus:border-foundry-light-primary dark:focus:border-foundry-primary focus:outline-none"
  />
  {#if slugPreview}
    <p class="font-mono text-[10px] text-foundry-light-muted dark:text-foundry-text/40">from the "assistant" template as: {slugPreview}</p>
  {/if}
  {#if error}
    <p class="text-[11px] text-red-500 dark:text-red-400">{error}</p>
  {/if}
  <div class="flex items-center gap-1.5">
    <button
      type="submit"
      class="flex-1 rounded-md bg-foundry-light-primary dark:bg-foundry-primary px-2 py-1 text-xs font-semibold text-white disabled:cursor-not-allowed disabled:opacity-50"
      disabled={submitting || !slugPreview}
    >
      {submitting ? 'Creating…' : 'Create'}
    </button>
    <button
      type="button"
      class="rounded-md px-2 py-1 text-xs text-foundry-light-muted dark:text-foundry-text/60 hover:bg-foundry-light-primary/10 dark:hover:bg-foundry-primary/10"
      on:click={() => dispatch('cancel')}
    >
      Cancel
    </button>
  </div>
</form>
