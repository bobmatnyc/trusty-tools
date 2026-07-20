<script>
  import AgentsHeader from './AgentsHeader.svelte';
  import Badge from '../../lib/Badge.svelte';
  import Button from '../../lib/Button.svelte';

  const projects = [
    {
      name: 'trusty-search', top: '#d97742', status: { tone: 'warning', spinner: true, label: '1 RUNNING' },
      meta: '~/code/trusty-search · rust\nmain · clean · 8 tasks done',
      tasks: [
        { dot: '#d9a83a', text: 'Fix flaky reindex e2e test', tag: 'IMPL', tagColor: '#d9a83a' },
        { dot: '#8fbf6a', text: 'Add disk-usage column', tag: 'DONE', tagColor: '#8fbf6a' }
      ],
      cta: { variant: 'primary', label: 'OPEN CHAT →' }
    },
    {
      name: 'trusty-memory', top: '#8a5a2b', status: { tone: 'success', dot: true, label: 'IDLE' },
      meta: '~/code/trusty-memory · rust\nmain · clean · 3 tasks done',
      tasks: [
        { dot: '#8fbf6a', text: 'Dream-phase compaction pass', tag: 'DONE', tagColor: '#8fbf6a' },
        { dot: '#8fbf6a', text: 'Palace export to JSONL', tag: 'DONE', tagColor: '#8fbf6a' }
      ],
      cta: { variant: '', label: 'OPEN CHAT →' }
    },
    {
      name: 'trusty-code', top: '#6e5843', status: { tone: 'danger', dot: true, label: '1 FAILED' },
      meta: '~/code/trusty-code · rust\nfeat/verify-gate · 2 dirty files',
      tasks: [
        { dot: '#e06a52', text: 'Bump usearch to 2.16', tag: 'FAILED', tagColor: '#e06a52' },
        { dot: '#a58a6b', text: 'Session resume from TUI', tag: 'QUEUED', tagColor: '#a58a6b' }
      ],
      cta: { variant: 'danger', label: 'REVIEW FAILURE →' }
    }
  ];
  const activity = [
    { t: '14:02', kind: 'IMPLEMENT', color: '#d9a83a', text: 'engineer editing tests/reindex_e2e.rs', proj: 'trusty-search' },
    { t: '13:58', kind: 'PLAN ✓', color: '#8fbf6a', text: 'PM approved plan for task #482', proj: 'trusty-search' },
    { t: '13:41', kind: 'VERIFY ✕', color: '#e06a52', text: 'cargo test failed — 2 regressions in usearch bump', proj: 'trusty-code' },
    { t: '12:10', kind: 'COMPLETE', color: '#8fbf6a', text: 'Dream-phase compaction pass merged', proj: 'trusty-memory' }
  ];
</script>

<div class="dark screen">
  <AgentsHeader tab="PROJECTS" />
  <div class="content">
    <div class="head">
      <h1 class="page-title">PROJECTS</h1>
      <div class="tools">
        <input class="input" placeholder="filter projects…">
        <Button variant="primary">+ LINK REPO</Button>
      </div>
    </div>
    <div class="subhead">3 LINKED REPOS · 12 TASKS THIS WEEK · PM LOOP ENABLED ON ALL</div>

    <div class="grid">
      {#each projects as p}
        <div class="card proj" style="border-top:3px solid {p.top};">
          <div class="p-head">
            <span class="p-name">{p.name}</span>
            <Badge tone={p.status.tone} dot={p.status.dot} spinner={p.status.spinner}>{p.status.label}</Badge>
          </div>
          <div class="p-meta">{p.meta}</div>
          <div class="p-tasks">
            {#each p.tasks as t}
              <div class="p-task">
                <span class="dot" style="background:{t.dot}"></span>
                <span class="truncate">{t.text}</span>
                <span class="tag-mini" style="color:{t.tagColor}">{t.tag}</span>
              </div>
            {/each}
          </div>
          <div class="p-act"><Button variant={p.cta.variant} size="sm">{p.cta.label}</Button></div>
        </div>
      {/each}
    </div>

    <div class="log card-dark">
      <div class="log-head">
        <span>RECENT ACTIVITY · ALL PROJECTS</span>
        <a href="#log">FULL LOG →</a>
      </div>
      <div class="log-rows">
        {#each activity as a}
          <div class="log-row">
            <span class="t">{a.t}</span>
            <span class="kind" style="color:{a.color}">{a.kind}</span>
            <span class="text">{a.text}</span>
            <span class="proj">{a.proj}</span>
          </div>
        {/each}
      </div>
    </div>
  </div>
</div>

<style>
  .screen { width: 1440px; height: 900px; display: flex; flex-direction: column; background: var(--trusty-content-bg); overflow: hidden; font-size: 14px; color: var(--trusty-text-primary); }
  .content { flex: 1; padding: 32px 40px; overflow: hidden; min-height: 0; }
  .head { display: flex; align-items: baseline; justify-content: space-between; margin-bottom: 8px; }
  h1 { font-size: 26px; margin: 0; }
  .tools { display: flex; gap: 10px; align-items: center; }
  .tools .input { width: 240px; padding: 8px 14px; font-size: 12px; }
  .subhead { font: 500 11px var(--trusty-mono); color: var(--trusty-text-muted); margin-bottom: 22px; }
  .grid { display: grid; grid-template-columns: repeat(3, 1fr); gap: 16px; margin-bottom: 24px; }
  .proj { padding: 20px; }
  .p-head { display: flex; justify-content: space-between; align-items: flex-start; margin-bottom: 10px; }
  .p-name { font: 700 16px var(--trusty-display); }
  .p-meta { font: 400 11px var(--trusty-mono); color: var(--trusty-text-muted); line-height: 1.9; margin-bottom: 14px; white-space: pre-line; }
  .p-tasks { display: flex; flex-direction: column; gap: 7px; padding-top: 12px; border-top: 1px solid var(--trusty-border); font-size: 12.5px; color: var(--trusty-text-secondary); }
  .p-task { display: flex; gap: 8px; align-items: center; }
  .p-task .truncate { flex: 1; }
  .dot { width: 7px; height: 7px; border-radius: 50%; flex: none; }
  .tag-mini { font: 500 10px var(--trusty-mono); }
  .p-act { margin-top: 16px; }
  .card-dark { background: var(--trusty-sidebar-bg); border: 1px solid var(--trusty-sidebar-border); border-radius: var(--trusty-radius); overflow: hidden; }
  .log-head { padding: 10px 18px; border-bottom: 1px solid var(--trusty-sidebar-border); display: flex; justify-content: space-between; align-items: center; font: 600 10px var(--trusty-mono); letter-spacing: 0.14em; color: var(--trusty-text-muted); }
  .log-head a { font: 600 10px var(--trusty-mono); letter-spacing: 0.08em; }
  .log-rows { padding: 6px 0; font: 400 12px var(--trusty-mono); }
  .log-row { display: flex; gap: 16px; padding: 8px 18px; color: var(--trusty-text-secondary); }
  .t { color: var(--trusty-text-muted); flex: 0 0 60px; }
  .kind { flex: 0 0 110px; }
  .text { flex: 1; }
  .proj { color: var(--trusty-text-muted); }
</style>
