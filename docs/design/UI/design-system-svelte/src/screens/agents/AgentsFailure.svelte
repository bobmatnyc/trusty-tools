<script>
  import AgentsHeader from './AgentsHeader.svelte';
  import Badge from '../../lib/Badge.svelte';
  import Button from '../../lib/Button.svelte';
  import RobotMark from '../../lib/RobotMark.svelte';
</script>

<div class="dark screen">
  <AgentsHeader subtitle="TASK #479 · BUMP USEARCH TO 2.16" showTabs={false}>
    {#snippet right()}
      <Badge tone="danger" dot>VERIFY FAILED</Badge>
    {/snippet}
  </AgentsHeader>

  <div class="body">
    <div class="thread">
      <div class="msg user"><div class="bubble">Bump usearch to 2.16 and make sure all index tests still pass.</div></div>
      <div class="msg bot">
        <RobotMark size={30} body="#2b1c12" face="#e06a52" eyes="visor" antenna={false} />
        <div class="stack">
          <div class="bubble">
            Implementation finished, but the VERIFY gate caught two regressions. The 2.16 API changed
            <code class="ref">Index::add</code> to take owned vectors — two call sites still pass references.
          </div>
          <div class="fail">
            <div class="fail-head"><span>✕ VERIFY FAILED · cargo test</span><span>2 FAILURES / 148 TESTS</span></div>
            <div class="fail-log">
              <div class="f">FAILED  index::tests::add_batch_borrowed</div>
              <div class="d">error[E0308]: mismatched types — expected Vec&lt;f32&gt;, found &amp;Vec&lt;f32&gt;</div>
              <div class="d">src/index/hnsw.rs:214</div>
              <div class="f gap">FAILED  index::tests::reindex_preserves_ids</div>
              <div class="d">src/index/reindex.rs:88 — same mismatch</div>
            </div>
            <div class="fail-acts">
              <Button variant="primary" size="sm">RETRY — FIX CALL SITES</Button>
              <Button size="sm">OPEN DIFF</Button>
              <Button variant="danger" size="sm">ROLL BACK BRANCH</Button>
            </div>
          </div>
          <div class="pm-note">
            PM recommendation: retry. The fix is mechanical — pass owned clones at both call sites.
            Estimated 1 phase, no plan change needed.
          </div>
        </div>
      </div>
    </div>
    <div class="composer">
      <textarea class="textarea" rows="2" placeholder="Reply to the PM… (Enter to send)"></textarea>
      <Button variant="primary">SEND</Button>
    </div>
  </div>
</div>

<style>
  .screen { width: 1440px; height: 900px; display: flex; flex-direction: column; background: var(--trusty-content-bg); overflow: hidden; font-size: 14px; color: var(--trusty-text-primary); }
  .body { flex: 1; display: flex; flex-direction: column; min-height: 0; max-width: 960px; width: 100%; margin: 0 auto; }
  .thread { flex: 1; padding: 28px 32px; display: flex; flex-direction: column; gap: 16px; overflow: hidden; }
  .msg { display: flex; }
  .msg.user { justify-content: flex-end; }
  .msg.bot { justify-content: flex-start; gap: 12px; }
  .msg.bot :global(.robot) { margin-top: 2px; border: 1.5px solid var(--trusty-border-strong); box-sizing: border-box; }
  .bubble { max-width: 64%; padding: 12px 16px; border-radius: 8px; line-height: 1.6; font-size: 13.5px; }
  .user .bubble { border-bottom-right-radius: 2px; background: #b7410e; color: #fff; }
  .stack { max-width: 76%; display: flex; flex-direction: column; gap: 10px; }
  .stack .bubble { max-width: none; border-bottom-left-radius: 2px; background: var(--trusty-card-bg); border: 1px solid var(--trusty-border); }
  .ref { font: 500 12px var(--trusty-mono); color: var(--trusty-sidebar-accent); }
  .fail { background: var(--trusty-card-bg); border: 1.5px solid #7a3428; border-radius: var(--trusty-radius); overflow: hidden; }
  .fail-head { padding: 9px 14px; background: var(--trusty-danger-soft); font: 600 10px var(--trusty-mono); letter-spacing: 0.14em; color: var(--trusty-danger); display: flex; justify-content: space-between; }
  .fail-log { padding: 12px 14px; font: 400 11.5px var(--trusty-mono); color: var(--trusty-text-secondary); line-height: 1.8; background: var(--trusty-sidebar-bg); }
  .f { color: var(--trusty-danger); }
  .f.gap { margin-top: 6px; }
  .d { color: var(--trusty-text-muted); padding-left: 16px; }
  .fail-acts { padding: 10px 14px; border-top: 1px solid var(--trusty-border); display: flex; gap: 8px; }
  .pm-note { padding: 10px 14px; border-radius: var(--trusty-radius); background: var(--trusty-warning-soft); border: 1px solid #6b5320; font-size: 12.5px; color: var(--trusty-warning); line-height: 1.55; }
  .composer { flex: none; padding: 16px 32px 20px; border-top: 1px solid var(--trusty-sidebar-border); display: flex; gap: 10px; align-items: flex-end; }
  .composer .textarea { min-height: 0; font-size: 13.5px; }
</style>
