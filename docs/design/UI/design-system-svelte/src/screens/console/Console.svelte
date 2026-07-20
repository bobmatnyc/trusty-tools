<script>
  import RobotMark from '../../lib/RobotMark.svelte';
  import Badge from '../../lib/Badge.svelte';
  import Button from '../../lib/Button.svelte';

  const services = [
    { name: 'Trusty Search', id: 'trusty-search', ver: 'VER 0.4.2 · PORT :7878', tone: 'success', status: 'RUNNING', mark: { body: '#b7410e', eyes: 'square' }, primary: true },
    { name: 'Trusty Memory', id: 'trusty-memory', ver: 'VER 0.3.8 · PORT :7882', tone: 'success', status: 'RUNNING', mark: { body: '#8a5a2b', eyes: 'round' }, primary: true },
    { name: 'Trusty Analyze', id: 'trusty-analyze', ver: 'VER 0.2.1 · PORT :7879', tone: 'warning', status: 'DEGRADED', mark: { body: '#6e5843', eyes: 'visor' }, note: 'Reachable, but console_metrics tool is missing.', noteWarn: true, primary: true },
    { name: 'Trusty Review', id: 'trusty-review', ver: 'VER 0.1.4', tone: 'warning', status: 'AVAILABLE', mark: { body: '#a3672e', eyes: 'square' }, note: 'Binary found but daemon is not running.' },
    { name: 'Trusty MPM', id: 'trusty-mpm', ver: 'VER 0.5.0 · 3 SESSIONS', tone: 'success', status: 'RUNNING', mark: { body: '#8a5a2b', eyes: 'square' }, primary: true }
  ];
  const tabs = ['OVERVIEW', 'SEARCH', 'MEMORY', 'ANALYZE', 'REVIEW', 'SESSIONS', 'CONFIG'];
</script>

<div class="screen">
  <div class="wrap">
    <div class="masthead">
      <div class="ident">
        <RobotMark size={46} body="#2b1c12" face="#e9b98a" antennaColor="#b7410e" state="idle" />
        <div>
          <div class="name">TRUSTY CONSOLE</div>
          <div class="sub">COMMAND DECK · UNIFIED SERVICE DASHBOARD</div>
        </div>
      </div>
      <div class="right">
        <div class="theme-toggle">
          <span class="on">DAY</span><span>NIGHT</span><span>AUTO</span>
        </div>
        <Badge tone="success" dot>6 SERVICES</Badge>
      </div>
    </div>

    <div class="tabs">
      {#each tabs as t, i}
        <span class="tab" class:active={i === 0}>{t}</span>
      {/each}
    </div>

    <div class="grid">
      {#each services as s}
        <div class="card svc">
          <div class="head">
            <div class="ident-sm">
              <RobotMark size={28} antenna={false} {...s.mark} />
              <span class="svc-name">{s.name}</span>
            </div>
            <Badge tone={s.tone} dot>{s.status}</Badge>
          </div>
          <div class="meta">ID {s.id}<br>{s.ver}</div>
          {#if s.note}<div class="note" class:warn={s.noteWarn}>{s.note}</div>{/if}
          <div class="act"><Button variant={s.primary ? 'primary' : ''} size="sm">VIEW DETAILS →</Button></div>
        </div>
      {/each}
      <div class="card svc absent">
        <div class="head">
          <div class="ident-sm">
            <RobotMark size={28} antenna={false} body="var(--trusty-surface-raised)" face="#a58a6b" eyes="visor" />
            <span class="svc-name dim">Trusty Code</span>
          </div>
          <Badge tone="muted">ABSENT</Badge>
        </div>
        <div class="meta">ID trusty-code</div>
        <div class="act">
          <button class="btn btn-sm installing"><span class="spinner"></span>INSTALLING… 64%</button>
        </div>
      </div>
    </div>
  </div>
</div>

<style>
  .screen { width: 1440px; height: 900px; background: var(--trusty-content-bg); overflow: hidden; font-size: 14px; color: var(--trusty-text-primary); }
  .wrap { max-width: 1160px; margin: 0 auto; padding: 36px 32px; }
  .masthead { display: flex; justify-content: space-between; align-items: center; margin-bottom: 24px; }
  .ident { display: flex; align-items: center; gap: 16px; }
  .name { font: 700 24px var(--trusty-display); letter-spacing: 0.02em; line-height: 1.1; }
  .sub { font: 500 11px var(--trusty-mono); letter-spacing: 0.14em; color: var(--trusty-text-muted); }
  .right { display: flex; align-items: center; gap: 10px; }
  .theme-toggle { display: flex; border: 1.5px solid var(--trusty-border-strong); border-radius: 4px; overflow: hidden; }
  .theme-toggle span { padding: 5px 12px; font: 600 11px var(--trusty-mono); background: var(--trusty-card-bg); color: var(--trusty-text-secondary); }
  .theme-toggle .on { background: var(--trusty-accent); color: #fff; }
  .tabs { display: flex; gap: 2px; border-bottom: 2px solid var(--trusty-border); margin-bottom: 24px; }
  .tab { padding: 10px 18px; font: 600 12px var(--trusty-mono); letter-spacing: 0.06em; color: var(--trusty-text-muted); }
  .tab.active { color: var(--trusty-accent); border-bottom: 3px solid var(--trusty-accent); margin-bottom: -2px; }
  .grid { display: grid; grid-template-columns: repeat(3, 1fr); gap: 16px; }
  .svc { padding: 20px; }
  .svc.absent { background: var(--trusty-content-bg); border-style: dashed; }
  .head { display: flex; justify-content: space-between; align-items: flex-start; margin-bottom: 12px; }
  .ident-sm { display: flex; align-items: center; gap: 10px; }
  .svc-name { font: 700 15px var(--trusty-display); }
  .svc-name.dim { color: var(--trusty-text-secondary); }
  .meta { font: 400 11px var(--trusty-mono); color: var(--trusty-text-muted); line-height: 1.9; }
  .note { margin-top: 8px; font-size: 12px; color: var(--trusty-text-secondary); }
  .note.warn { color: #7a5a10; }
  .act { margin-top: 14px; }
  .installing { display: inline-flex; align-items: center; gap: 8px; background: var(--trusty-surface-raised); border-color: var(--trusty-border); color: var(--trusty-text-secondary); }
</style>
