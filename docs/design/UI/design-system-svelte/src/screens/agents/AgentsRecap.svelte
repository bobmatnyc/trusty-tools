<script>
  import Button from '../../lib/Button.svelte';

  const phases = [
    { name: 'RESEARCH', state: 'done', time: '13:44 → 13:51 · 7m · researcher', note: 'Reproduced the timeout locally (1 in 6 runs). Isolated race between SSE stream assertion and daemon batch flush.' },
    { name: 'PLAN', state: 'done', time: '13:51 → 13:58 · 7m · PM + user', note: 'Replace fixed 5s sleep with event-driven wait on the complete event, 30s bound. Approved without changes.' },
    { name: 'IMPLEMENT', state: 'running', time: '13:58 → now · 12m · engineer', note: 'Editing tests/reindex_e2e.rs — event-wait helper added, replacing sleep at 3 call sites (2 of 3 done).' },
    { name: 'VERIFY', state: 'queued', time: 'queued · qa', note: 'Will run the e2e suite 20× to confirm flake is gone.' }
  ];
  const files = [
    { op: 'M', opColor: '#d9a83a', path: 'tests/reindex_e2e.rs', add: '+41', del: '−18' },
    { op: 'A', opColor: '#8fbf6a', path: 'tests/support/event_wait.rs', add: '+66', del: '−0' },
    { op: 'M', opColor: '#d9a83a', path: 'src/index/stream.rs', add: '+3', del: '−1' }
  ];
  const agents = [
    { name: 'engineer', dot: '#d9a83a', note: 'editing · 12m active · 28.4k tokens' },
    { name: 'researcher', dot: '#8fbf6a', note: 'done · 7m active · 9.8k tokens' },
    { name: 'qa', dot: '#a58a6b', note: 'waiting on IMPLEMENT' }
  ];
</script>

<div class="dark screen">
  <header>
    <div class="left">
      <span class="back">← BACK TO CHAT</span>
      <span class="brand">SESSION RECAP</span>
      <span class="sub">TASK #482 · FIX FLAKY REINDEX E2E TEST</span>
    </div>
    <div class="acts">
      <Button size="sm">EXPORT MARKDOWN</Button>
      <Button variant="primary" size="sm">SAVE TO MEMORY</Button>
    </div>
  </header>

  <div class="body">
    <div class="main">
      <section>
        <div class="s-label">PHASE TIMELINE</div>
        <div class="card timeline">
          {#each phases as p, i}
            <div class="ph">
              <div class="rail">
                <span class="node {p.state}">
                  {#if p.state === 'done'}✓{:else if p.state === 'running'}<span class="spinner"></span>{:else}○{/if}
                </span>
                {#if i < phases.length - 1}<span class="line"></span>{/if}
              </div>
              <div class="ph-body" class:pad={i < phases.length - 1}>
                <div class="ph-head">
                  <span class="ph-name" class:mute={p.state === 'queued'}>{p.name}</span>
                  <span class="ph-time" class:warn={p.state === 'running'}>{p.time}</span>
                </div>
                <div class="ph-note" class:mute={p.state === 'queued'}>{p.note}</div>
              </div>
            </div>
          {/each}
        </div>
      </section>
      <section>
        <div class="s-label">FILES TOUCHED</div>
        <div class="files card-dark">
          {#each files as f}
            <div class="file">
              <span class="op" style="color:{f.opColor}">{f.op}</span>
              <span class="path">{f.path}</span>
              <span class="add">{f.add}</span>
              <span class="del">{f.del}</span>
            </div>
          {/each}
        </div>
      </section>
    </div>

    <aside>
      <section>
        <div class="s-label">AGENTS</div>
        <div class="agents">
          {#each agents as a}
            <div class="agent card">
              <span class="dot" style="background:{a.dot}"></span>
              <div>
                <div class="a-name">{a.name}</div>
                <div class="a-note">{a.note}</div>
              </div>
            </div>
          {/each}
        </div>
      </section>
      <section>
        <div class="s-label">BUDGET</div>
        <div class="budget">
          <div>
            <div class="b-row"><span>TOKENS</span><span class="dim">42.1k / 200k · 21%</span></div>
            <div class="track"><span style="width:21%; background:var(--trusty-accent);"></span></div>
          </div>
          <div>
            <div class="b-row"><span>WALL CLOCK</span><span class="dim">26m / 60m cap</span></div>
            <div class="track"><span style="width:43%; background:var(--trusty-warning);"></span></div>
          </div>
          <div class="b-row"><span>EST. COST</span><span class="dim">$0.84</span></div>
        </div>
      </section>
      <section>
        <div class="s-label">MEMORY WRITES</div>
        <div class="mem card">
          2 drawers queued for palace <code class="ref">trusty-search/testing</code>: flake root cause;
          event-wait helper pattern.
        </div>
      </section>
    </aside>
  </div>
</div>

<style>
  .screen { width: 1440px; height: 900px; display: flex; flex-direction: column; background: var(--trusty-content-bg); overflow: hidden; font-size: 14px; color: var(--trusty-text-primary); }
  header { height: 52px; flex: none; background: var(--trusty-sidebar-bg); border-bottom: 1px solid var(--trusty-sidebar-border); display: flex; align-items: center; justify-content: space-between; padding: 0 20px; }
  .left { display: flex; align-items: center; gap: 12px; }
  .back { font: 600 12px var(--trusty-mono); color: var(--trusty-text-muted); }
  .brand { font: 700 15px var(--trusty-display); letter-spacing: 0.04em; }
  .sub { font: 500 10px var(--trusty-mono); letter-spacing: 0.12em; color: var(--trusty-text-muted); }
  .acts { display: flex; gap: 8px; }
  .body { flex: 1; display: flex; min-height: 0; }
  .main { flex: 1; padding: 28px 32px; display: flex; flex-direction: column; gap: 20px; overflow: hidden; min-width: 0; }
  .s-label { font: 600 10px var(--trusty-mono); letter-spacing: 0.14em; color: var(--trusty-sidebar-accent); margin-bottom: 10px; }
  .timeline { padding: 18px 20px; }
  .ph { display: flex; gap: 14px; }
  .rail { display: flex; flex-direction: column; align-items: center; }
  .node { width: 22px; height: 22px; border-radius: 50%; font: 600 11px var(--trusty-mono); display: flex; align-items: center; justify-content: center; }
  .node.done { background: var(--trusty-success-soft); border: 1.5px solid var(--trusty-success); color: var(--trusty-success); }
  .node.running { background: var(--trusty-warning-soft); border: 1.5px solid var(--trusty-warning); color: var(--trusty-warning); }
  .node.running .spinner { width: 8px; height: 8px; }
  .node.queued { border: 1.5px solid var(--trusty-border-strong); color: var(--trusty-text-muted); }
  .line { width: 2px; flex: 1; background: var(--trusty-border); min-height: 26px; }
  .ph-body.pad { padding-bottom: 16px; }
  .ph-head { display: flex; gap: 10px; align-items: baseline; }
  .ph-name { font: 600 12px var(--trusty-mono); }
  .ph-name.mute { color: var(--trusty-text-muted); }
  .ph-time { font: 400 10px var(--trusty-mono); color: var(--trusty-text-muted); }
  .ph-time.warn { color: var(--trusty-warning); }
  .ph-note { font-size: 12.5px; color: var(--trusty-text-secondary); line-height: 1.55; margin-top: 4px; }
  .ph-note.mute { color: var(--trusty-text-muted); }
  .card-dark { background: var(--trusty-sidebar-bg); border: 1px solid var(--trusty-sidebar-border); border-radius: var(--trusty-radius); font: 400 12px var(--trusty-mono); }
  .file { display: flex; gap: 14px; padding: 9px 16px; color: var(--trusty-text-secondary); border-bottom: 1px solid var(--trusty-sidebar-border); }
  .file:last-child { border-bottom: none; }
  .op { width: 14px; }
  .path { flex: 1; }
  .add { color: var(--trusty-success); }
  .del { color: var(--trusty-danger); }
  aside { width: 340px; flex: none; background: var(--trusty-sidebar-bg); border-left: 1px solid var(--trusty-sidebar-border); padding: 28px 24px; display: flex; flex-direction: column; gap: 22px; overflow: hidden; }
  .agents { display: flex; flex-direction: column; gap: 10px; }
  .agent { display: flex; align-items: center; gap: 10px; padding: 10px 12px; }
  .dot { width: 8px; height: 8px; border-radius: 50%; flex: none; }
  .a-name { font: 600 12px var(--trusty-mono); }
  .a-note { font: 400 10px var(--trusty-mono); color: var(--trusty-text-muted); }
  .budget { display: flex; flex-direction: column; gap: 12px; }
  .b-row { display: flex; justify-content: space-between; margin-bottom: 5px; font: 400 11px var(--trusty-mono); }
  .dim { color: var(--trusty-text-muted); }
  .track { height: 7px; background: var(--trusty-surface-raised); border-radius: 2px; overflow: hidden; }
  .track span { display: block; height: 100%; }
  .mem { font-size: 12px; color: var(--trusty-text-secondary); line-height: 1.6; padding: 10px 12px; }
  .ref { font: 500 11px var(--trusty-mono); color: var(--trusty-sidebar-accent); }
</style>
