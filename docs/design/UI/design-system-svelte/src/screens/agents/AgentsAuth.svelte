<script>
  import RobotMark from '../../lib/RobotMark.svelte';
  import Button from '../../lib/Button.svelte';

  let token = $state('sk-ant-api03-••••••••••••');
  let endpoint = $state('http://localhost:8790');
</script>

<div class="dark screen">
  <span class="corner">TRUSTY WORKS ⚙ EST. 2026</span>
  <div class="card gate">
    <div class="ident">
      <RobotMark size={64} body="#d97742" face="#201612" state="receiving" />
      <div class="titles">
        <div class="name">TRUSTY AGENTS</div>
        <div class="sub">UNIT-04 REQUESTS CREDENTIALS</div>
      </div>
    </div>
    <div class="form-group">
      <label class="form-label lit" for="token">ANTHROPIC API TOKEN</label>
      <input id="token" class="input" type="password" bind:value={token}>
      <div class="hint">Stored locally in the OS keychain. Never leaves this machine except to api.anthropic.com.</div>
    </div>
    <div class="form-group last">
      <label class="form-label lit" for="endpoint">MPM ENDPOINT</label>
      <input id="endpoint" class="input" bind:value={endpoint}>
      <div class="reach"><span class="dot"></span>DAEMON REACHABLE · trusty-mpm v0.5.0</div>
    </div>
    <Button variant="primary">CONNECT →</Button>
    <div class="links">
      <a href="#help">Where do I get a token?</a>
      <a href="#skip" class="dim">Skip — offline mode</a>
    </div>
  </div>
</div>

<style>
  .screen { width: 1440px; height: 900px; display: flex; align-items: center; justify-content: center; background: var(--trusty-content-bg); overflow: hidden; font-size: 14px; color: var(--trusty-text-primary); position: relative; }
  .corner { position: absolute; top: 20px; left: 24px; font: 500 10px var(--trusty-mono); letter-spacing: 0.14em; color: var(--trusty-border-strong); }
  .gate { width: 480px; padding: 40px 44px; border-radius: var(--trusty-radius-lg); }
  .ident { display: flex; flex-direction: column; align-items: center; gap: 14px; margin-bottom: 28px; }
  .titles { text-align: center; }
  .name { font: 700 22px var(--trusty-display); letter-spacing: 0.03em; }
  .sub { font: 500 10px var(--trusty-mono); letter-spacing: 0.14em; color: var(--trusty-text-muted); margin-top: 4px; }
  .lit { color: var(--trusty-sidebar-accent); }
  .hint { font-size: 11.5px; color: var(--trusty-text-muted); line-height: 1.5; margin-top: 6px; }
  .last { margin-bottom: 22px; }
  .reach { display: flex; align-items: center; gap: 7px; font: 600 10px var(--trusty-mono); color: var(--trusty-success); margin-top: 8px; }
  .dot { width: 6px; height: 6px; border-radius: 50%; background: var(--trusty-success); }
  .gate :global(.btn-primary) { width: 100%; justify-content: center; padding: 12px 0; font-size: 13px; letter-spacing: 0.08em; }
  .links { margin-top: 16px; padding-top: 16px; border-top: 1px solid var(--trusty-border); display: flex; justify-content: space-between; font: 500 11px var(--trusty-mono); }
  .links .dim { color: var(--trusty-text-muted); }
</style>
