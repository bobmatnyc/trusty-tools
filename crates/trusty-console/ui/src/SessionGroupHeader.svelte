<!--
  One lifecycle group's heading, as a disclosure control (#8282).

  Why: the Sessions tab folds its inactive groups away by default, so the
  heading stopped being static text — it is now the only way back to the rows
  underneath it. That makes it a real `<button>`: keyboard-operable for free,
  and carrying `aria-expanded` / `aria-controls` so a screen reader announces
  the state and can reach the region it controls. A heading that toggled on a
  bare `<h3 onclick>` would do neither.
  What: renders the group name, its row count (which stays visible while the
  group is collapsed — the count is the whole point of a collapsed header) and a
  disclosure triangle. It owns no state: `collapsed` comes in, `onToggle` goes
  out, and `sessionRows.js` decides both.
  Test: `sessionsTabCollapse.test.js` — "the group header is a button carrying
  aria-expanded and aria-controls".
-->
<script>
  let { group, count, collapsed = false, controls, onToggle } = $props();
</script>

<h3 class="group-title {group}">
  <button
    type="button"
    class="group-toggle"
    aria-expanded={!collapsed}
    aria-controls={controls}
    onclick={onToggle}
  >
    <span class="disclosure" class:collapsed aria-hidden="true">▾</span>
    <span class="group-name">{group}</span>
    <span class="group-count">({count})</span>
  </button>
</h3>

<style>
  .group-title {
    font-size: 0.85rem; font-weight: 600; text-transform: uppercase;
    letter-spacing: 0.05em; margin: 1rem 0 0.5rem; color: var(--trusty-text-secondary);
  }
  .group-title.active { color: var(--trusty-success); }
  .group-title.errored { color: var(--trusty-danger); }
  .group-title.deleted { color: var(--trusty-text-muted); }
  .group-title.other { color: var(--trusty-warning); }

  .group-toggle {
    display: flex; align-items: center; gap: 0.4rem;
    background: none; border: none; padding: 0.15rem 0; margin: 0;
    color: inherit; font: inherit; letter-spacing: inherit; text-transform: inherit;
    cursor: pointer;
  }
  .group-toggle:focus-visible {
    outline: 2px solid var(--trusty-accent); outline-offset: 2px; border-radius: 0.25rem;
  }
  .disclosure {
    font-size: 0.7rem; line-height: 1; transition: transform 120ms ease-out;
  }
  /* Rotated rather than swapped for a second glyph, so the two states are the
     same width and the heading does not shift as it toggles. */
  .disclosure.collapsed { transform: rotate(-90deg); }
  .group-count { color: var(--trusty-text-muted); font-weight: 500; }
</style>
