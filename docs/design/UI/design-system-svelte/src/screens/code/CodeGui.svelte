<script>
  import AgentsHeader from '../agents/AgentsHeader.svelte';
  import RobotMark from '../../lib/RobotMark.svelte';
  import Badge from '../../lib/Badge.svelte';
  import Button from '../../lib/Button.svelte';

  const sessions = [
    { title: '#482 Fix flaky reindex e2e', status: 'IMPLEMENT', tone: '#d9a83a', spinner: true, when: '1m 12s', active: true },
    { title: '#481 Disk-usage column', status: '✓ MERGED', tone: '#8fbf6a', when: '1h ago' },
    { title: '#479 Bump usearch 2.16', status: '✕ FAILED', tone: '#e06a52', when: 'yesterday' }
  ];
  const output = [
    { t: '09:14:03', kind: 'TOOL', kindColor: '#e9b98a', text: 'read tests/reindex_e2e.rs' },
    { t: '09:14:11', kind: 'TOOL', kindColor: '#e9b98a', text: 'edit tests/reindex_e2e.rs' },
    { t: '09:14:19', kind: 'TOOL', kindColor: '#e9b98a', text: 'cargo test reindex_e2e' },
    { t: '', kind: '', text: '3 passed · 0 failed · 4.2s', ok: true },
    { t: '09:14:31', kind: 'INFO', kindColor: '#8fbf6a', text: 'engineer → qa handoff' },
    { t: '09:14:31', kind: 'PM', kindColor: '#d97742', text: 'ready for VERIFY', cursor: true }
  ];
</script>

<div class="dark screen">
  <header>
    <div class="left">
      <RobotMark size={30} body="#d97742" face="#201612" state="working" />
      <span class="brand">TRUSTY CODE</span>
      <span class="sub">CODING HARNESS · trusty-tools · feat/foundry-ds</span>
    </div>
    <div class="right">
      <span class="chip warn">IMPLEMENT 3/4</span>
      <Badge tone="success" dot>SERVE :8790</Badge>
    </div>
  </header>

  <div class="body">
    <aside>
      <div class="new"><Button variant="primary">+ NEW TASK</Button></div>
      <div class="s-label">SESSIONS</div>
      <div class="list">
        {#each sessions as s}
          <div class="sess" class:active={s.active}>
            <div class="s-title" class:dim={!s.active}>{s.title}</div>
            <div class="s-meta">
              <span class="s-status" style="color:{s.tone}">
                {#if s.spinner}<span class="spinner"></span>{/if}{s.status}
              </span>
              <span class="s-when">{s.when}</span>
            </div>
          </div>
        {/each}
      </div>
      <div class="foot">AGENTS <span class="amber">engineer●</span> qa○<br>TOKENS <span class="lit">42.1k/200k</span></div>
    </aside>

    <div class="main">
      <div class="phasebar-wrap">
        <div class="phasebar">
          <div class="seg"><span class="ok">✓</span><span class="pname">RESEARCH</span><span class="conn done"></span></div>
          <div class="seg"><span class="ok">✓</span><span class="pname">PLAN</span><span class="conn done"></span></div>
          <div class="seg"><span class="spinner amber-sp"></span><span class="pname lit">IMPLEMENT</span><span class="conn"></span></div>
          <div class="seg end"><span class="mute">○</span><span class="pname mute">VERIFY</span></div>
        </div>
      </div>

      <div class="panes">
        <div class="pane diff">
          <div class="pane-head">
            <span class="file">tests/reindex_e2e.rs</span>
            <span class="stats"><i class="add">+3</i> <i class="del">−2</i> · 1 of 2 files</span>
          </div>
          <div class="code">
            <div class="hunk">@@ -88,7 +88,8 @@ async fn reindex_completes()</div>
            <div class="ctx">    daemon.trigger_reindex("fixture").await?;</div>
            <div class="rm">-   tokio::time::sleep(Duration::from_secs(5)).await;</div>
            <div class="rm">-   assert!(stream.saw("complete"));</div>
            <div class="ad">+   // Event-driven wait, bounded — no more fixed-sleep race (#482).</div>
            <div class="ad">+   wait_for_event(&amp;mut stream, "complete", TIMEOUT_30S).await?;</div>
            <div class="ad">+   assert_eq!(stream.last_event(), "complete");</div>
            <div class="ctx">    daemon.shutdown().await;</div>
          </div>
        </div>
        <div class="pane">
          <div class="pane-head label">SESSION OUTPUT</div>
          <div class="log">
            {#each output as o}
              <div class:indent={o.ok} class:okline={o.ok}>
                {#if o.t}<span class="t" class:pm={o.kind === 'PM'}>{o.t}</span> <span style="color:{o.kindColor}">{o.kind}</span> {/if}{o.text}{#if o.cursor}<span class="cursor"></span>{/if}
              </div>
            {/each}
          </div>
        </div>
      </div>

      <div class="gate">
        <span class="gate-label">VERIFY GATE</span>
        <span class="gate-text">Tests pass locally. Approve to run the full verify phase (CI matrix + lint) and open the PR.</span>
        <Button size="sm">REQUEST CHANGES</Button>
        <Button variant="primary" size="sm">APPROVE &amp; VERIFY</Button>
      </div>
    </div>
  </div>
</div>

<style>
  .screen { width: 1440px; height: 900px; display: flex; flex-direction: column; background: var(--trusty-content-bg); overflow: hidden; font-size: 14px; color: var(--trusty-text-primary); }
  header { height: 52px; flex: none; background: var(--trusty-sidebar-bg); border-bottom: 1px solid var(--trusty-sidebar-border); display: flex; align-items: center; justify-content: space-between; padding: 0 20px; }
  .left, .right { display: flex; align-items: center; gap: 12px; }
  .right { gap: 10px; }
  .brand { font: 700 15px var(--trusty-display); letter-spacing: 0.04em; }
  .sub { font: 500 10px var(--trusty-mono); letter-spacing: 0.12em; color: var(--trusty-text-muted); }
  .chip.warn { padding: 2px 8px; border-radius: 4px; font: 600 10px var(--trusty-mono); background: var(--trusty-warning-soft); color: var(--trusty-warning); border: 1px solid var(--trusty-border-strong); }
  .body { flex: 1; display: flex; min-height: 0; }
  aside { width: 264px; flex: none; background: var(--trusty-sidebar-bg); border-right: 1px solid var(--trusty-sidebar-border); display: flex; flex-direction: column; }
  .new { padding: 14px 14px 10px; }
  .new :global(.btn) { width: 100%; justify-content: center; }
  .s-label { padding: 6px 14px; font: 600 10px var(--trusty-mono); letter-spacing: 0.14em; color: var(--trusty-text-muted); }
  .list { flex: 1; padding: 0 8px; display: flex; flex-direction: column; gap: 3px; }
  .sess { padding: 10px 12px; border-radius: 4px; border-left: 3px solid transparent; }
  .sess.active { background: var(--trusty-surface-raised); border-left-color: var(--trusty-accent); }
  .s-title { font-size: 13px; font-weight: 600; margin-bottom: 3px; }
  .s-title.dim { font-weight: 500; color: var(--trusty-text-secondary); }
  .s-meta { display: flex; gap: 8px; }
  .s-status { display: inline-flex; align-items: center; gap: 5px; font: 600 9px var(--trusty-mono); }
  .s-status .spinner { width: 8px; height: 8px; }
  .s-when { font: 500 10px var(--trusty-mono); color: var(--trusty-text-muted); }
  .foot { padding: 12px 14px; border-top: 1px solid var(--trusty-sidebar-border); font: 500 10px var(--trusty-mono); color: var(--trusty-text-muted); line-height: 1.9; }
  .amber { color: #d9a83a; }
  .lit { color: var(--trusty-sidebar-accent); }
  .main { flex: 1; display: flex; flex-direction: column; min-width: 0; }
  .phasebar-wrap { flex: none; padding: 16px 24px 0; }
  .phasebar { display: flex; align-items: center; background: var(--trusty-sidebar-bg); border: 1px solid var(--trusty-sidebar-border); border-radius: 5px; padding: 12px 18px; }
  .seg { display: flex; align-items: center; gap: 9px; flex: 1; }
  .seg.end { flex: none; }
  .ok { font: 600 11px var(--trusty-mono); color: var(--trusty-success); }
  .mute { font: 600 11px var(--trusty-mono); color: var(--trusty-text-muted); }
  .pname { font: 600 11px var(--trusty-mono); color: var(--trusty-text-secondary); }
  .pname.lit { color: var(--trusty-text-primary); }
  .pname.mute { color: var(--trusty-text-muted); }
  .conn { flex: 1; height: 2px; background: var(--trusty-surface-raised); margin: 0 6px; }
  .conn.done { background: var(--trusty-success); }
  .amber-sp { width: 10px; height: 10px; color: var(--trusty-warning); }
  .panes { flex: 1; display: flex; gap: 16px; padding: 16px 24px; min-height: 0; }
  .pane { flex: 1; background: var(--trusty-sidebar-bg); border: 1px solid var(--trusty-sidebar-border); border-radius: 5px; overflow: hidden; display: flex; flex-direction: column; min-width: 0; }
  .pane.diff { flex: 1.3; }
  .pane-head { padding: 10px 16px; border-bottom: 1px solid var(--trusty-sidebar-border); display: flex; justify-content: space-between; align-items: center; }
  .pane-head.label { font: 600 10px var(--trusty-mono); letter-spacing: 0.14em; color: var(--trusty-text-muted); }
  .file { font: 600 11px var(--trusty-mono); color: var(--trusty-sidebar-accent); }
  .stats { font: 500 10px var(--trusty-mono); color: var(--trusty-text-muted); }
  .stats i { font-style: normal; }
  .add { color: var(--trusty-success); }
  .del { color: var(--trusty-danger); }
  .code { flex: 1; padding: 12px 16px; font: 400 12px var(--trusty-mono); line-height: 1.85; overflow: hidden; }
  .hunk { color: var(--trusty-text-muted); }
  .ctx { color: var(--trusty-text-secondary); }
  .rm { background: var(--trusty-danger-soft); color: var(--trusty-danger); }
  .ad { background: var(--trusty-success-soft); color: var(--trusty-success); }
  .log { flex: 1; padding: 12px 16px; font: 400 11.5px var(--trusty-mono); line-height: 1.95; overflow: hidden; color: var(--trusty-text-secondary); }
  .t { color: var(--trusty-text-muted); }
  .t.pm { color: var(--trusty-accent); }
  .indent { padding-left: 70px; }
  .okline { color: var(--trusty-success); }
  .cursor { display: inline-block; width: 7px; height: 13px; background: var(--trusty-accent); vertical-align: text-bottom; margin-left: 4px; animation: tc-blink 1s step-end infinite; }
  @keyframes tc-blink { 50% { opacity: 0; } }
  .gate { flex: none; margin: 0 24px 20px; background: var(--trusty-surface-raised); border: 1px solid var(--trusty-border-strong); border-radius: 5px; padding: 12px 18px; display: flex; align-items: center; gap: 14px; }
  .gate-label { font: 600 11px var(--trusty-mono); letter-spacing: 0.12em; color: var(--trusty-sidebar-accent); }
  .gate-text { font-size: 13px; color: var(--trusty-text-secondary); flex: 1; }
</style>
