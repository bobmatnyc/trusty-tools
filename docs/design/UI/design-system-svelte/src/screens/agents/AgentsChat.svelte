<script>
  import AgentsHeader from './AgentsHeader.svelte';
  import Button from '../../lib/Button.svelte';
  import RobotMark from '../../lib/RobotMark.svelte';

  const history = [
    { title: 'Fix flaky reindex e2e test', status: 'IMPLEMENTING', tone: '#d9a83a', spinner: true, when: '2m ago', active: true },
    { title: 'Add disk-usage column to Indexes', status: '✓ COMPLETE', tone: '#8fbf6a', when: '1h ago' },
    { title: 'Migrate tokens.css to Foundry', status: '✓ COMPLETE', tone: '#8fbf6a', when: '3h ago' },
    { title: 'Bump usearch to 2.16', status: '✕ FAILED', tone: '#e06a52', when: 'yesterday' }
  ];
  const phases = [
    { name: 'RESEARCH', state: 'done', note: 'root cause isolated' },
    { name: 'PLAN', state: 'done', note: 'approved by PM' },
    { name: 'IMPLEMENT', state: 'running', note: 'engineer agent editing tests/reindex_e2e.rs' },
    { name: 'VERIFY', state: 'queued', note: 'queued' }
  ];
  const agents = [
    { name: 'engineer', dot: '#d9a83a', note: 'editing 1 file' },
    { name: 'researcher', dot: '#8fbf6a', note: 'idle' },
    { name: 'qa', dot: '#a58a6b', note: 'waiting' }
  ];
</script>

<div class="dark screen">
  <AgentsHeader tab="CHAT" />
  <div class="body">
    <aside class="left">
      <div class="new"><Button variant="primary">+ NEW TASK</Button></div>
      <div class="section-label">TASK HISTORY</div>
      <div class="list">
        {#each history as h}
          <div class="task" class:active={h.active}>
            <div class="t-title" class:dim={!h.active}>{h.title}</div>
            <div class="t-meta">
              <span class="t-status" style="color:{h.tone}">
                {#if h.spinner}<span class="spinner"></span>{/if}{h.status}
              </span>
              <span class="t-when">{h.when}</span>
            </div>
          </div>
        {/each}
      </div>
      <div class="foot">PM LOOP · RESEARCH→PLAN→IMPLEMENT→VERIFY</div>
    </aside>

    <div class="center">
      <div class="thread">
        <div class="msg user"><div class="bubble">Fix the flaky reindex e2e test — it times out on CI about once in five runs.</div></div>
        <div class="msg bot">
          <RobotMark size={30} body="#2b1c12" face="#e9b98a" antenna={false} state="working" />
          <div class="stack">
            <div class="bubble">
              Research complete. The timeout is in <code class="ref">tests/reindex_e2e.rs</code> — the SSE
              stream assertion races the daemon's batch flush. Plan: replace the fixed 5s sleep with an
              event-driven wait on the <code class="ref">complete</code> event, bounded at 30s.
            </div>
            <div class="workflow">
              <div class="wf-head"><span>WORKFLOW · TASK #482</span><span class="phase-n">PHASE 3/4</span></div>
              <div class="wf-body">
                {#each phases as p}
                  <div class="wf-row">
                    <span class="wf-ic">
                      {#if p.state === 'done'}<span class="ok">✓</span>
                      {:else if p.state === 'running'}<span class="spinner run"></span>
                      {:else}<span class="idle">○</span>{/if}
                    </span>
                    <span class="wf-name" class:lit={p.state === 'running'} class:mute={p.state === 'queued'}>{p.name}</span>
                    <span class="wf-note">{p.note}</span>
                  </div>
                {/each}
              </div>
            </div>
          </div>
        </div>
      </div>
      <div class="composer">
        <textarea class="textarea" rows="2" placeholder="Describe a task for the PM… (Enter to send)"></textarea>
        <Button variant="primary">SEND</Button>
      </div>
    </div>

    <aside class="right">
      <div class="r-head">SESSION RECAP</div>
      <div class="r-body">
        <div>
          <div class="r-label">AGENTS ACTIVE</div>
          {#each agents as a}
            <div class="agent"><span class="dot" style="background:{a.dot}"></span><span class="a-name">{a.name}</span><span class="a-note">{a.note}</span></div>
          {/each}
        </div>
        <div>
          <div class="r-label">FILES TOUCHED</div>
          <div class="files">tests/reindex_e2e.rs<br>src/index/stream.rs</div>
        </div>
        <div>
          <div class="r-label">TOKENS</div>
          <div class="tok-row"><span>42.1k / 200k</span><span class="pct">21%</span></div>
          <div class="bar wide"><span class="bar-fill" style="width:21%"></span></div>
        </div>
      </div>
    </aside>
  </div>
</div>

<style>
  .screen { width: 1440px; height: 900px; display: flex; flex-direction: column; background: var(--trusty-content-bg); overflow: hidden; font-size: 14px; color: var(--trusty-text-primary); }
  .body { flex: 1; display: flex; min-height: 0; }
  aside.left { width: 280px; flex: none; background: var(--trusty-sidebar-bg); border-right: 1px solid var(--trusty-sidebar-border); display: flex; flex-direction: column; }
  .new { padding: 16px 16px 10px; }
  .new :global(.btn) { width: 100%; justify-content: center; }
  .section-label { padding: 8px 16px; font: 600 10px var(--trusty-mono); letter-spacing: 0.14em; color: var(--trusty-text-muted); }
  .list { flex: 1; overflow: hidden; padding: 0 10px; display: flex; flex-direction: column; gap: 3px; }
  .task { padding: 10px 12px; border-radius: 4px; border-left: 3px solid transparent; }
  .task.active { background: var(--trusty-surface-raised); border-left-color: var(--trusty-accent); }
  .t-title { font-size: 13px; font-weight: 600; margin-bottom: 3px; }
  .t-title.dim { font-weight: 500; color: var(--trusty-text-secondary); }
  .t-meta { display: flex; gap: 8px; align-items: center; }
  .t-status { display: inline-flex; align-items: center; gap: 5px; font: 600 9px var(--trusty-mono); }
  .t-status .spinner { width: 8px; height: 8px; }
  .t-when { font: 500 10px var(--trusty-mono); color: var(--trusty-text-muted); }
  .foot { padding: 14px 16px; border-top: 1px solid var(--trusty-sidebar-border); font: 500 10px var(--trusty-mono); color: var(--trusty-text-muted); letter-spacing: 0.12em; }
  .center { flex: 1; display: flex; flex-direction: column; min-width: 0; }
  .thread { flex: 1; padding: 24px 32px; display: flex; flex-direction: column; gap: 16px; overflow: hidden; }
  .msg { display: flex; }
  .msg.user { justify-content: flex-end; }
  .msg.bot { justify-content: flex-start; gap: 12px; }
  .msg.bot :global(.robot) { margin-top: 2px; border: 1.5px solid var(--trusty-border-strong); box-sizing: border-box; }
  .bubble { max-width: 64%; padding: 12px 16px; border-radius: 8px; line-height: 1.6; font-size: 13.5px; }
  .user .bubble { border-bottom-right-radius: 2px; background: #b7410e; color: #fff; }
  .bot .bubble, .bot .stack { max-width: 72%; }
  .bot .stack .bubble { max-width: none; }
  .stack { display: flex; flex-direction: column; gap: 10px; }
  .bot .bubble { border-bottom-left-radius: 2px; background: var(--trusty-card-bg); border: 1px solid var(--trusty-border); }
  .ref { font: 500 12px var(--trusty-mono); color: var(--trusty-sidebar-accent); }
  .workflow { background: var(--trusty-card-bg); border: 1px solid var(--trusty-border); border-radius: var(--trusty-radius); overflow: hidden; }
  .wf-head { padding: 9px 14px; background: var(--trusty-surface-raised); font: 600 10px var(--trusty-mono); letter-spacing: 0.14em; color: var(--trusty-sidebar-accent); display: flex; justify-content: space-between; }
  .phase-n { color: var(--trusty-warning); }
  .wf-body { padding: 12px 14px; display: flex; flex-direction: column; gap: 9px; }
  .wf-row { display: flex; align-items: center; gap: 10px; }
  .wf-ic { width: 16px; }
  .ok { font: 600 10px var(--trusty-mono); color: var(--trusty-success); }
  .idle { font: 600 10px var(--trusty-mono); color: var(--trusty-text-muted); }
  .spinner.run { width: 9px; height: 9px; color: var(--trusty-warning); }
  .wf-name { font: 500 12px var(--trusty-mono); color: var(--trusty-text-secondary); }
  .wf-name.lit { color: var(--trusty-text-primary); }
  .wf-name.mute { color: var(--trusty-text-muted); }
  .wf-note { font: 400 11px var(--trusty-mono); color: var(--trusty-text-muted); }
  .composer { flex: none; padding: 16px 32px 20px; border-top: 1px solid var(--trusty-sidebar-border); display: flex; gap: 10px; align-items: flex-end; }
  .composer .textarea { min-height: 0; font-size: 13.5px; }
  aside.right { width: 300px; flex: none; background: var(--trusty-sidebar-bg); border-left: 1px solid var(--trusty-sidebar-border); display: flex; flex-direction: column; }
  .r-head { padding: 14px 18px; border-bottom: 1px solid var(--trusty-sidebar-border); font: 600 10px var(--trusty-mono); letter-spacing: 0.14em; color: var(--trusty-text-muted); }
  .r-body { padding: 16px 18px; display: flex; flex-direction: column; gap: 14px; font-size: 12.5px; color: var(--trusty-text-secondary); line-height: 1.55; }
  .r-label { font: 600 10px var(--trusty-mono); letter-spacing: 0.12em; color: var(--trusty-sidebar-accent); margin-bottom: 5px; }
  .agent { display: flex; align-items: center; gap: 8px; margin-bottom: 7px; }
  .dot { width: 7px; height: 7px; border-radius: 50%; flex: none; }
  .a-name { font: 500 11px var(--trusty-mono); }
  .a-note { font: 400 10px var(--trusty-mono); color: var(--trusty-text-muted); }
  .files { font: 400 11px var(--trusty-mono); line-height: 1.8; }
  .tok-row { display: flex; justify-content: space-between; margin-bottom: 5px; font: 400 11px var(--trusty-mono); }
  .pct { color: var(--trusty-text-muted); }
  .bar.wide { width: 100%; height: 7px; border: none; background: var(--trusty-surface-raised); border-radius: 2px; }
</style>
