<script>
  // Gallery of the four standard dialogs, rendered inline (no backdrop).
  import Modal from '../../lib/Modal.svelte';
  import Button from '../../lib/Button.svelte';
</script>

<div class="board">
  <Modal title="STOP THE DAEMON?" backdrop={false}>
    <p>UNIT-01 will power down. Active searches are interrupted and MCP clients lose their connection.</p>
    <div class="cmd">restart: trusty-search stop &amp;&amp; trusty-search start</div>
    {#snippet footer()}
      <Button>CANCEL</Button>
      <Button variant="primary">STOP DAEMON</Button>
    {/snippet}
  </Modal>

  <Modal title="REGISTER A NEW INDEX" backdrop={false}>
    <div class="form-group">
      <label class="form-label" for="ix-id">INDEX ID</label>
      <input id="ix-id" class="input" value="docs-site">
    </div>
    <div class="form-group">
      <label class="form-label" for="ix-path">ROOT PATH</label>
      <input id="ix-path" class="input focus" value="/Users/mo/code/docs-site">
      <div class="hint">Absolute path. Indexing starts immediately after registration.</div>
    </div>
    <label class="check"><input type="checkbox" checked> Boost results from the current git branch</label>
    {#snippet footer()}
      <Button>CANCEL</Button>
      <Button variant="primary">CREATE &amp; INDEX</Button>
    {/snippet}
  </Modal>

  <Modal title="INDEX SETTINGS" titleMeta="trusty-tools" backdrop={false}>
    <div class="form-group">
      <span class="form-label">IGNORE GLOBS</span>
      <div class="globs">
        <span class="tag">target/** <i>✕</i></span>
        <span class="tag">node_modules/** <i>✕</i></span>
        <span class="tag">*.lock <i>✕</i></span>
        <span class="add">add glob…</span>
      </div>
    </div>
    <div class="pair">
      <div class="form-group">
        <label class="form-label" for="chunk">MAX CHUNK SIZE</label>
        <input id="chunk" class="input" value="1024">
      </div>
      <div class="form-group">
        <label class="form-label" for="model">EMBEDDING MODEL</label>
        <select id="model" class="select"><option>bge-small-en-v1.5</option></select>
      </div>
    </div>
    <label class="check"><input type="checkbox" checked> Knowledge-graph expansion (caller/callee chains)</label>
    <label class="check"><input type="checkbox"> Watch filesystem and reindex changed files</label>
    {#snippet footer()}
      <div class="spread">
        <Button variant="ghost">RESET TO DEFAULTS</Button>
        <div class="pairbtn">
          <Button>CANCEL</Button>
          <Button variant="primary">SAVE SETTINGS</Button>
        </div>
      </div>
    {/snippet}
  </Modal>

  <Modal title="REINDEX FAILED" danger backdrop={false}>
    <p><code class="ref">gitflow-rs</code> could not be reindexed — the registered root path no longer exists on disk.</p>
    <div class="log">
      <span class="t">2026-07-18T09:14:02Z</span> <span class="err">ERROR</span> indexer: walk failed<br>
      <span class="t">  path:</span> /Users/mo/code/gitflow-rs<br>
      <span class="t">  cause:</span> No such file or directory (os error 2)
    </div>
    {#snippet footer()}
      <div class="spread">
        <Button variant="ghost">OPEN LOGS</Button>
        <div class="pairbtn">
          <Button variant="danger">REMOVE INDEX</Button>
          <Button variant="primary">EDIT ROOT PATH</Button>
        </div>
      </div>
    {/snippet}
  </Modal>
</div>

<style>
  .board { width: 1440px; background: var(--trusty-surface-raised); padding: 48px; box-sizing: border-box; display: grid; grid-template-columns: 1fr 1fr; gap: 40px; align-items: start; font-size: 14px; color: var(--trusty-text-primary); }
  .board :global(.modal) { box-shadow: var(--trusty-shadow-lg); }
  p { margin: 0 0 10px; font-size: 13.5px; line-height: 1.6; color: var(--trusty-text-secondary); }
  .cmd { padding: 10px 14px; background: var(--trusty-content-bg); border: 1px solid var(--trusty-surface-raised); border-radius: 4px; font: 400 12px var(--trusty-mono); color: var(--trusty-text-muted); }
  .hint { font-size: 12px; color: var(--trusty-text-muted); margin-top: 6px; }
  .focus { border-color: var(--trusty-accent); box-shadow: 0 0 0 3px var(--trusty-accent-soft); }
  .check { display: flex; align-items: center; gap: 10px; font-size: 13px; color: var(--trusty-text-secondary); margin-bottom: 10px; }
  .check input { width: 15px; height: 15px; }
  .globs { display: flex; flex-wrap: wrap; gap: 6px; padding: 8px 10px; border: 1.5px solid var(--trusty-border-strong); border-radius: 4px; background: var(--trusty-card-bg); }
  .tag i { font-style: normal; color: var(--trusty-sidebar-muted); }
  .add { font: 400 12px var(--trusty-mono); color: var(--trusty-sidebar-muted); padding: 2px 4px; }
  .pair { display: grid; grid-template-columns: 1fr 1fr; gap: 16px; }
  .spread { display: flex; justify-content: space-between; align-items: center; width: 100%; }
  .pairbtn { display: flex; gap: 8px; }
  .ref { font: 600 12px var(--trusty-mono); color: var(--trusty-accent); }
  .log { padding: 12px 14px; background: #2b1c12; border-radius: 4px; font: 400 11.5px var(--trusty-mono); color: #e6d8c8; line-height: 1.7; }
  .log .t { color: #a58a6b; }
  .log .err { color: #f0a898; }
</style>
