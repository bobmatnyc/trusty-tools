<script>
  /*
   * Why: the search box suggests symbols and files while the operator types,
   * so a known name is one keystroke and an Enter away.
   * What: an ARIA 1.2 combobox — the input owns the listbox through
   * `aria-controls` and points at the active option with
   * `aria-activedescendant`, so focus never leaves the input. Typing schedules
   * a debounced fan-out (`createTypeahead`); ArrowDown/ArrowUp move through the
   * options, Enter on an option fills the box with its label and runs the
   * search, Enter with no option runs the search as typed, and Escape closes
   * the list. A 501 from every index (the console has no typeahead route
   * yet) turns suggestions off for the session.
   * Test: `SearchBox.test.js`.
   */
  import { onDestroy } from 'svelte';
  import { createTypeahead, fanOutTypeahead, isUnroutable } from '../typeahead.js';

  /** @type {{ value?: string, indexIds?: string[], onsearch: (q: string) => void, placeholder?: string }} */
  let { value = $bindable(''), indexIds = [], onsearch, placeholder = '' } = $props();

  const uid = $props.id();
  const listId = `${uid}-suggestions`;

  let suggestions = $state([]);
  let active = $state(-1);
  let open = $state(false);
  let focused = $state(false);
  let unroutable = $state(false);

  const typeahead = createTypeahead({
    load: (q, signal) => fanOutTypeahead(indexIds, q, signal),
    onResults: (hits) => {
      suggestions = hits;
      active = -1;
      open = focused && hits.length > 0;
    },
    onError: (e) => {
      if (isUnroutable(e)) unroutable = true;
      suggestions = [];
      open = false;
    }
  });
  onDestroy(() => typeahead.cancel());

  let expanded = $derived(open && suggestions.length > 0);

  function onInput(e) {
    if (unroutable || indexIds.length === 0) return;
    typeahead.schedule(e.currentTarget.value);
  }

  function close() {
    open = false;
    active = -1;
  }

  function choose(hit) {
    typeahead.cancel();
    value = hit.label;
    close();
    onsearch(value);
  }

  function onKeydown(e) {
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      if (suggestions.length === 0) return;
      e.preventDefault();
      if (!expanded) {
        open = true;
        active = e.key === 'ArrowDown' ? 0 : suggestions.length - 1;
        return;
      }
      const step = e.key === 'ArrowDown' ? 1 : -1;
      active = (active + step + suggestions.length) % suggestions.length;
    } else if (e.key === 'Enter') {
      e.preventDefault();
      if (expanded && active >= 0) {
        choose(suggestions[active]);
      } else {
        typeahead.cancel();
        close();
        onsearch(value);
      }
    } else if (e.key === 'Escape') {
      if (expanded) {
        e.preventDefault();
        close();
      }
    }
  }
</script>

<div class="searchbox">
  <input
    type="text"
    class="input"
    role="combobox"
    aria-label="Search query"
    aria-autocomplete="list"
    aria-expanded={expanded}
    aria-controls={listId}
    aria-activedescendant={expanded && active >= 0 ? `${uid}-opt-${active}` : undefined}
    autocomplete="off"
    {placeholder}
    bind:value
    oninput={onInput}
    onkeydown={onKeydown}
    onfocus={() => (focused = true)}
    onblur={() => {
      focused = false;
      close();
    }}
  />
  <ul id={listId} class="suggestions" role="listbox" aria-label="Suggestions" hidden={!expanded}>
    {#each suggestions as hit, i (`${hit.index_id}\0${hit.path}\0${hit.start_line}`)}
      <!-- Keyboard selection lives on the input (aria-activedescendant, the
           ARIA 1.2 combobox pattern), so options take no key handler; the
           mousedown guard keeps a click from blurring the input first. -->
      <!-- svelte-ignore a11y_click_events_have_key_events -->
      <li
        id={`${uid}-opt-${i}`}
        role="option"
        aria-selected={i === active}
        class:active={i === active}
        onmousedown={(e) => e.preventDefault()}
        onclick={() => choose(hit)}
      >
        <span class="label text-mono">{hit.label}</span>
        <span class="where text-xs">{hit.path}:{hit.start_line}</span>
        <span class="badge badge-muted">{hit.index_id}</span>
      </li>
    {/each}
  </ul>
</div>

<style>
  .searchbox {
    position: relative;
    flex: 1;
    min-width: 0;
  }
  .searchbox .input {
    width: 100%;
  }
  .suggestions {
    position: absolute;
    z-index: 20;
    top: calc(100% + 2px);
    left: 0;
    right: 0;
    margin: 0;
    padding: var(--trusty-space-1) 0;
    list-style: none;
    background: var(--trusty-card-bg);
    border: 1px solid var(--trusty-border);
    border-radius: var(--trusty-radius);
    box-shadow: var(--trusty-shadow);
    max-height: 320px;
    overflow-y: auto;
  }
  .suggestions[hidden] {
    display: none;
  }
  li {
    display: flex;
    align-items: center;
    gap: var(--trusty-space-2);
    padding: var(--trusty-space-2) var(--trusty-space-3);
    cursor: pointer;
  }
  li.active,
  li:hover {
    background: var(--trusty-surface-hover);
  }
  .label {
    font-weight: 600;
    color: var(--trusty-text-primary);
  }
  .where {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--trusty-text-muted);
  }
</style>
