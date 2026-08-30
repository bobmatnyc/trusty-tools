<script>
  /**
   * Why: Displays a single service's status card; delegates tab-awareness to
   *      the caller (App.svelte) which owns SERVICE_TAB_MAP — the single source
   *      of truth for which services have real detail tabs.
   * What: Renders service name, status badge, optional hint, and — since #6370 —
   *       makes the WHOLE card the click target when the card offers exactly one
   *       action. `cardActions.js` owns that rule; a card with two or more
   *       actions keeps a discrete button per action and stays inert itself.
   *       A clickable card carries role="button", tabindex=0 and an
   *       Enter/Space keydown handler, so it is reachable and operable from the
   *       keyboard rather than being a div that only a mouse can use. Its
   *       aria-label replaces the name the contents would compute, so
   *       aria-describedby points back at the status badge, the version and the
   *       hint — otherwise a screen reader announces the service and its action
   *       and never says the card is degraded.
   * Test: `cardActions.test.js` covers the one-action / many-action / no-action
   *       rule, the Enter/Space key set, and the described-by composition.
   *       `statusPresentation.test.js` covers the badge label, badge color and
   *       hint sentence, including the on-demand members (#6416).
   *       Ordering of the cards themselves is
   *       server-side — see `detect::order_for_display` and
   *       `test_services_route_orders_running_before_absent`.
   *
   * @typedef {{ id: string, display_name: string, status: string, version?: string, url?: string, hint?: string, lifecycle?: string }} Service
   * @type {{ service: Service, tabbedServices: Set<string>, onViewDetails?: (id: string) => void }}
   */
  import {
    cardActivation,
    cardDescribedBy,
    isActivationKey,
    sanitizeElementId,
  } from './cardActions.js';
  import { cardPresentation } from './statusPresentation.js';

  let { service, tabbedServices = new Set(), onViewDetails } = $props();

  // #6416: label, badge color and hint sentence come from one pure function, so
  // the "Available means a stopped daemon" reading stops being hard-coded into
  // this markup — it is wrong for trusty-review and trusty-analyze. The toneVar
  // it returns is a `var(--trusty-*)` reference that resolves against the active
  // palette at render time, so badges recolor when the theme flips.
  let presentation = $derived(cardPresentation(service));
  let statusLabel = $derived(presentation.label);
  let statusVar = $derived(presentation.toneVar);
  // hasTab is derived from the caller-owned tabbedServices — no local duplicate.
  let hasTab = $derived(tabbedServices.has(service.id));

  function handleViewDetails() {
    onViewDetails?.(service.id);
  }

  // #6370: the card's actions, in display order. One entry today; the shape is
  // what decides between whole-card click and discrete buttons.
  let actions = $derived(
    hasTab && onViewDetails
      ? [{ id: 'details', label: 'View details →', run: handleViewDetails }]
      : [],
  );
  let activation = $derived(cardActivation(actions));
  // #6370: the elements a screen reader reads as this card's description.
  let describedBy = $derived(cardDescribedBy(service));
  // The same helper `cardDescribedBy` derives its ids with, so the ids this
  // markup emits and the ids that description points at cannot drift apart.
  let elementId = $derived(sanitizeElementId(service.id));

  /**
   * Activate the card from the keyboard.
   *
   * Space is prevented from its default so activating a card does not also
   * scroll the Overview grid.
   */
  function handleCardKeydown(event) {
    if (!isActivationKey(event.key)) return;
    event.preventDefault();
    activation.primary?.run();
  }
</script>

{#snippet cardContents()}
  <div class="card-header">
    <h2 class="name">{service.display_name}</h2>
    <span class="badge" id="svc-{elementId}-status" style="--_s: {statusVar};">
      <span class="dot"></span>
      {statusLabel}
    </span>
  </div>

  <div class="card-body">
    <p class="id">ID: <code>{service.id}</code></p>
    {#if service.version}
      <p class="version" id="svc-{elementId}-version">Version: <code>{service.version}</code></p>
    {/if}
    {#if presentation.hint?.kind === 'install'}
      <p class="hint" id="svc-{elementId}-hint">Install with <code>{presentation.hint.text}</code></p>
    {:else if presentation.hint}
      <p
        class="hint"
        class:degraded-hint={presentation.hint.kind === 'degraded'}
        id="svc-{elementId}-hint"
      >{presentation.hint.text}</p>
    {/if}
  </div>
{/snippet}

{#if activation.mode === 'card'}
  <!-- One action → the whole card is the button. The cue below is decorative:
       the accessible name comes from aria-label, so a screen reader announces
       the service and its action once, not twice. aria-describedby then adds
       the status, version and hint the label leaves out. -->
  <div
    class="card card-clickable"
    role="button"
    tabindex="0"
    aria-label="{service.display_name} — {activation.primary.label}"
    aria-describedby={describedBy}
    onclick={activation.primary.run}
    onkeydown={handleCardKeydown}
  >
    {@render cardContents()}
    <span class="details-cue" aria-hidden="true">{activation.primary.label}</span>
  </div>
{:else}
  <!-- Zero actions → a static status tile. Two or more → one button each, and
       the card itself stays non-interactive because no single action stands
       for it. -->
  <div class="card">
    {@render cardContents()}
    {#if activation.mode === 'buttons'}
      <div class="card-actions">
        {#each actions as action (action.id)}
          <button class="details-btn" onclick={action.run}>{action.label}</button>
        {/each}
      </div>
    {/if}
  </div>
{/if}

<style>
  .card {
    background: var(--trusty-card-bg);
    border: 1px solid var(--trusty-border);
    border-radius: 0.75rem;
    padding: 1.25rem;
    transition: border-color 0.15s;
  }
  .card:hover {
    border-color: var(--trusty-border-strong);
  }
  /* The whole-card click target (#6370). The pointer and the accent border on
     hover are the only signal that the body is clickable, so both are required
     — a card that navigates but looks inert is worse than a button. */
  .card-clickable {
    cursor: pointer;
    display: block;
    width: 100%;
    text-align: left;
  }
  .card-clickable:hover {
    border-color: var(--trusty-accent);
  }
  .card-clickable:focus-visible {
    outline: 2px solid var(--trusty-accent);
    outline-offset: 2px;
  }
  .card-header {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    gap: 0.5rem;
    margin-bottom: 0.75rem;
  }
  .name {
    font-size: 1.1rem;
    font-weight: 600;
    margin: 0;
    color: var(--trusty-text-primary);
  }
  .badge {
    display: flex;
    align-items: center;
    gap: 0.35rem;
    font-size: 0.75rem;
    font-weight: 600;
    padding: 0.2rem 0.6rem;
    border-radius: 9999px;
    border: 1px solid;
    white-space: nowrap;
    /* --_s is supplied inline (statusVar) as a theme-adaptive --trusty-status-* ref.
       The 13%/27% color-mix produces the subtle bg/border tint on either palette. */
    --_s: var(--trusty-text-muted);
    color: var(--_s);
    background: rgba(0,0,0,0.08);
    background: color-mix(in srgb, var(--_s) 13%, transparent);
    border-color: rgba(0,0,0,0.18);
    border-color: color-mix(in srgb, var(--_s) 27%, transparent);
  }
  .dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--_s);
  }
  .card-body p {
    margin: 0.3rem 0;
    font-size: 0.85rem;
    color: var(--trusty-text-secondary);
  }
  code {
    font-family: 'JetBrains Mono', 'Fira Code', monospace;
    font-size: 0.8rem;
    background: var(--trusty-surface-raised);
    padding: 0.1rem 0.35rem;
    border-radius: 0.25rem;
    color: var(--trusty-text-primary);
  }
  .hint {
    font-style: italic;
  }
  .degraded-hint {
    color: var(--trusty-status-degraded);
  }
  .details-cue {
    display: inline-block;
    margin-top: 0.75rem;
    color: var(--trusty-accent);
    font-size: 0.8rem;
    font-weight: 500;
  }
  .card-actions {
    display: flex;
    flex-wrap: wrap;
    gap: 0.5rem;
    margin-top: 0.75rem;
  }
  .details-btn {
    background: none;
    border: 1px solid var(--trusty-border-strong);
    border-radius: 0.4rem;
    color: var(--trusty-accent);
    cursor: pointer;
    font-size: 0.8rem;
    font-weight: 500;
    padding: 0.3rem 0.75rem;
    transition: background 0.15s, border-color 0.15s;
  }
  .details-btn:hover {
    background: rgba(0,0,0,0.06);
    background: color-mix(in srgb, var(--trusty-accent) 9%, transparent);
    border-color: var(--trusty-accent);
  }
</style>
