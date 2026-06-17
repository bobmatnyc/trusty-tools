<script>
  /*
   * Why: Several index-hygiene config fields (skip dirs, extensions, exclude
   * globs) are string lists. A tag-style editor — type + Enter to add, click ×
   * to remove — is friendlier and less error-prone than a comma-separated text
   * box, and keeps the parent view focused on orchestration rather than
   * per-field input plumbing (issue #1372).
   * What: A controlled tag editor. `items` is the bound string array; the
   * component adds trimmed, de-duplicated entries on Enter/comma/blur and emits
   * the new array through `bind:items`. Empty input is ignored.
   * Test: type "data" + Enter, assert `items` becomes ['data']; type "data"
   * again, assert no duplicate is added; click the × on a tag, assert removal.
   */
  let {
    items = $bindable([]),
    placeholder = 'Add an entry…',
    label = '',
    help = '',
    id = ''
  } = $props();

  let draft = $state('');

  function add() {
    const v = draft.trim();
    draft = '';
    if (v === '') return;
    if (items.includes(v)) return;
    items = [...items, v];
  }

  function removeAt(i) {
    items = items.filter((_, idx) => idx !== i);
  }

  function onKeydown(e) {
    if (e.key === 'Enter' || e.key === ',') {
      e.preventDefault();
      add();
    } else if (e.key === 'Backspace' && draft === '' && items.length > 0) {
      // Quick-delete the last tag when the input is empty.
      removeAt(items.length - 1);
    }
  }
</script>

<div class="form-group">
  {#if label}
    <label class="form-label" for={id}>{label}</label>
  {/if}
  <div class="tag-box">
    {#each items as item, i (item)}
      <span class="tag">
        <span class="tag-text">{item}</span>
        <button
          type="button"
          class="tag-remove"
          aria-label={`Remove ${item}`}
          onclick={() => removeAt(i)}>×</button
        >
      </span>
    {/each}
    <input
      {id}
      type="text"
      class="tag-input"
      {placeholder}
      bind:value={draft}
      onkeydown={onKeydown}
      onblur={add}
    />
  </div>
  {#if help}
    <div class="tag-help">{help}</div>
  {/if}
</div>

<style>
  .tag-box {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    align-items: center;
    padding: 6px 8px;
    border: 1px solid var(--trusty-border);
    border-radius: var(--trusty-radius-sm);
    background: var(--trusty-content-bg, #fff);
    min-height: 38px;
  }
  .tag {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    background: var(--trusty-primary-soft, rgba(59, 130, 246, 0.12));
    color: var(--trusty-text);
    border-radius: var(--trusty-radius-sm);
    padding: 2px 4px 2px 8px;
    font-size: var(--trusty-fs-xs);
  }
  .tag-text {
    font-family: var(--trusty-font-mono, monospace);
  }
  .tag-remove {
    border: none;
    background: transparent;
    cursor: pointer;
    color: var(--trusty-text-muted);
    font-size: 14px;
    line-height: 1;
    padding: 0 2px;
  }
  .tag-remove:hover {
    color: var(--trusty-danger);
  }
  .tag-input {
    flex: 1;
    min-width: 120px;
    border: none;
    outline: none;
    background: transparent;
    font-size: var(--trusty-fs-sm);
    padding: 4px 2px;
  }
  .tag-help {
    margin-top: var(--trusty-space-2);
    font-size: var(--trusty-fs-xs);
    color: var(--trusty-text-muted);
  }
</style>
