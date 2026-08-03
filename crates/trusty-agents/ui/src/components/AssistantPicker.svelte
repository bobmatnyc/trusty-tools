<script lang="ts">
  /**
   * Why (#4404, milestone M2): the app booted straight into a chat pane, so the
   * first question — WHO am I working with — was answerable only through a
   * header dropdown the user had to know was there. This is the landing view: a
   * card per assistant INSTANCE (Izzie, the CTO Assistant, anything the user
   * created), a card for the Concierge, and a create action.
   *
   * Selection is one line — `activeAgentId.set(decodeAssistantSelection(id))` —
   * because #4281 made `activeAgentId` itself the persistence surface: it seeds
   * from localStorage at import and writes through on every change. There is no
   * save call here to forget, and deliberately no picker-owned storage key. The
   * decode is NOT optional: the Concierge card carries the id `ctrl` (matching
   * the persisted sentinel and the config surface), and writing that literal
   * onto the dispatch axis would route through the tools-OFF persona path and
   * strip Concierge's delegation capability — see `lib/assistantPicker.ts`.
   *
   * Card ART is out of scope. The owner's decision is manual upload only with
   * generation as a fast-follow, and the generation path is entangled with
   * #4405's undecided model choice; no avatar field exists in this data model
   * yet. Cards therefore carry a deterministic monogram tile — a typographic
   * stand-in, not generated art, and no new dependency.
   *
   * The create flow REUSES `AddAgentForm` (the `POST /api/agents` "assistant"
   * template flow) rather than duplicating it, and refreshes the roster on
   * success so the new instance appears as a card immediately.
   * What: a responsive card grid. Clicking a card selects that assistant and
   * dispatches `select`, which `App.svelte` uses to move to the chat view.
   * Test: `AssistantPicker.test.ts`.
   */
  import { createEventDispatcher, onMount } from 'svelte';
  import { Plus } from 'lucide-svelte';
  import {
    activeAgentId,
    agentRoster,
    fetchAgentCatalog,
    refreshOverlayAgents,
  } from '../stores/app';
  import {
    avatarHue,
    buildPickerCards,
    decodeAssistantSelection,
    monogram,
  } from '../lib/assistantPicker';
  import AddAgentForm from './AddAgentForm.svelte';

  const dispatch = createEventDispatcher<{ select: { id: string | null } }>();

  let addingAgent = false;

  $: cards = buildPickerCards($agentRoster);
  // The card whose identity matches the live selection. Compared on the ROSTER
  // axis (`ctrl` for Concierge), which is what the cards carry — comparing the
  // raw `$activeAgentId` would leave the Concierge card unmarked whenever it is
  // the active one, i.e. on every fresh install.
  $: selectedCardId = $activeAgentId ?? 'ctrl';

  /**
   * Why: the picker is the landing view, so on a cold start it renders before
   * the sidecar is listening — the same race `lib/catalogRefetch.ts` documents
   * for the header pickers. `App.svelte` re-drives both catalog loads when the
   * API becomes healthy, which backfills this component through its reactive
   * stores; this mount-time call covers the already-warm case (returning to the
   * picker from chat) without waiting for that edge.
   * What: best-effort refresh; a failure leaves the Concierge card, which is
   * always selectable, rather than blocking the view behind an error.
   */
  onMount(() => {
    fetchAgentCatalog().catch((e) =>
      console.error('[AssistantPicker] fetchAgentCatalog failed:', e),
    );
    refreshOverlayAgents();
  });

  function select(cardId: string) {
    const id = decodeAssistantSelection(cardId);
    activeAgentId.set(id);
    dispatch('select', { id });
  }

  /**
   * Why: a newly created assistant the user cannot immediately use is a dead
   * end — the create flow's whole point is starting a conversation with it. The
   * catalog refresh has to complete BEFORE selecting, or `reconcileSelectedAssistant`
   * (a subscription on the merged roster, #4281) would see a populated roster
   * that does not yet contain the new id and demote the selection to Concierge.
   * What: refresh both roster sources, then select the new agent and leave the
   * picker.
   */
  async function onCreated(name: string) {
    addingAgent = false;
    try {
      await fetchAgentCatalog();
    } catch (e) {
      console.error('[AssistantPicker] fetchAgentCatalog after create failed:', e);
    }
    await refreshOverlayAgents();
    select(name);
  }
</script>

<div class="flex min-h-0 flex-1 flex-col gap-4 overflow-y-auto p-6">
  <div class="flex flex-col gap-1">
    <h1 class="text-lg font-semibold text-foundry-light-text dark:text-foundry-text">
      Who are you working with?
    </h1>
    <p class="text-xs text-foundry-light-muted dark:text-foundry-text/60">
      Pick an assistant to open its conversation. The choice is remembered across launches.
    </p>
  </div>

  <div class="grid grid-cols-[repeat(auto-fill,minmax(15rem,1fr))] gap-3">
    {#each cards as card (card.id)}
      <button
        type="button"
        aria-pressed={card.id === selectedCardId}
        class="flex flex-col gap-2 rounded-lg border p-4 text-left transition-colors {card.id ===
        selectedCardId
          ? 'border-foundry-light-primary dark:border-foundry-primary bg-foundry-light-primary/10 dark:bg-foundry-primary/10'
          : 'border-foundry-light-border dark:border-foundry-border hover:bg-foundry-light-primary/5 dark:hover:bg-foundry-primary/5'}"
        on:click={() => select(card.id)}
      >
        <div class="flex items-center gap-3">
          <!-- Monogram stand-in for card art (#4404 scope note in the header
               comment). `aria-hidden` because the label beside it already
               carries the identity — the tile must not be announced twice. -->
          <span
            class="flex h-10 w-10 shrink-0 items-center justify-center rounded-md font-mono text-sm font-semibold text-white"
            style="background-color: hsl({avatarHue(card.id)} 45% 42%)"
            aria-hidden="true"
          >
            {monogram(card.label)}
          </span>
          <span class="min-w-0 flex-1">
            <span
              class="block truncate text-sm font-semibold text-foundry-light-text dark:text-foundry-text"
            >
              {card.label}
            </span>
            <span
              class="block font-mono text-[10px] uppercase tracking-wide text-foundry-light-muted dark:text-foundry-text/50"
            >
              {card.origin === 'concierge'
                ? 'System tool'
                : card.origin === 'overlay'
                  ? 'Your assistant'
                  : 'Assistant'}
            </span>
          </span>
        </div>
        {#if card.description}
          <p class="text-[11px] leading-relaxed text-foundry-light-muted dark:text-foundry-text/60">
            {card.description}
          </p>
        {/if}
      </button>
    {/each}

    <!-- Create: the same POST /api/agents "assistant" template flow the header
         switcher uses, not a second implementation of it. -->
    <div
      class="flex flex-col justify-center gap-2 rounded-lg border border-dashed border-foundry-light-border dark:border-foundry-border p-4"
    >
      {#if addingAgent}
        <AddAgentForm
          on:created={(e) => onCreated(e.detail.name)}
          on:cancel={() => (addingAgent = false)}
        />
      {:else}
        <button
          type="button"
          class="flex items-center gap-2 text-sm font-semibold text-foundry-light-muted dark:text-foundry-text/60 hover:text-foundry-light-primary dark:hover:text-foundry-primary"
          on:click={() => (addingAgent = true)}
        >
          <Plus size={16} aria-hidden="true" />
          New assistant
        </button>
        <p class="text-[11px] text-foundry-light-muted dark:text-foundry-text/40">
          Creates a new instance from the assistant template.
        </p>
      {/if}
    </div>
  </div>
</div>
