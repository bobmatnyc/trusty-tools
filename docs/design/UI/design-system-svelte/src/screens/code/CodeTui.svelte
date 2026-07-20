<script>
  // Faithful HTML rendering of the tcode terminal UI (Foundry ANSI palette).
</script>

<div class="tui">
  <pre class="mark">  ┌─[◠]─┐
  │ ■ ■ │  TCODE v0.6.0 — CODING HARNESS
  │ ───── │  project: trusty-tools · branch: feat/foundry-ds
  └──────┘</pre>
  <div class="rule"></div>
  <div class="cols">
    <div class="panel side">
      <div class="p-label">TASK #482 · WORKFLOW</div>
      <div><span class="green">✓ RESEARCH</span> <span class="mute">· 42s · root cause isolated</span></div>
      <div><span class="green">✓ PLAN</span> <span class="mute">· 18s · approved</span></div>
      <div><span class="amber">▶ IMPLEMENT</span> <span class="fg">· running 1m 12s</span></div>
      <div class="mute nest">└ engineer → tests/reindex_e2e.rs</div>
      <div class="mute">○ VERIFY · queued</div>
      <div class="side-foot">AGENTS: <span class="amber">engineer●</span> researcher○ qa○<br>TOKENS: <span class="peach">42.1k/200k</span> ▮▮▮▯▯▯▯▯▯▯</div>
    </div>
    <div class="panel out">
      <div class="p-label">SESSION OUTPUT</div>
      <div><span class="mute">09:14:02</span> <span class="green">INFO</span> pm: phase transition PLAN → IMPLEMENT</div>
      <div><span class="mute">09:14:03</span> <span class="peach">TOOL</span> engineer: read tests/reindex_e2e.rs (214 lines)</div>
      <div><span class="mute">09:14:11</span> <span class="peach">TOOL</span> engineer: edit tests/reindex_e2e.rs</div>
      <div class="rm ind">- tokio::time::sleep(Duration::from_secs(5)).await;</div>
      <div class="green ind">+ wait_for_event(&amp;mut stream, "complete", TIMEOUT_30S).await?;</div>
      <div><span class="mute">09:14:19</span> <span class="peach">TOOL</span> engineer: cargo test -p trusty-search reindex_e2e</div>
      <div class="mute ind">running 3 tests… <span class="green">ok ok ok</span> (4.2s)</div>
      <div><span class="mute">09:14:31</span> <span class="green">INFO</span> engineer: implementation complete, handing to qa</div>
      <div><span class="rust">09:14:31</span> <span class="rust">PM</span> awaiting VERIFY phase<span class="cursor"></span></div>
    </div>
  </div>
  <div class="rule"></div>
  <div class="statusbar">
    <span class="mode">IMPLEMENT</span>
    <span><span class="peach">^P</span> phases</span><span><span class="peach">^A</span> agents</span><span><span class="peach">^L</span> logs</span><span><span class="peach">^C</span> abort</span>
    <span class="push">tcode serve · :8790 · MCP ok</span>
  </div>
</div>

<style>
  .tui { width: 1440px; height: 900px; display: flex; flex-direction: column; background: #171009; overflow: hidden; font: 400 13px var(--trusty-mono); color: #e6d8c8; padding: 20px 24px; box-sizing: border-box; line-height: 1.75; }
  .mark { color: #d97742; white-space: pre; line-height: 1.3; font-weight: 600; margin: 0; font-family: inherit; }
  .rule { height: 1px; background: #382513; margin: 14px 0; flex: none; }
  .cols { flex: 1; display: flex; gap: 20px; min-height: 0; }
  .panel { border: 1px solid #382513; border-radius: 3px; padding: 12px 16px; display: flex; flex-direction: column; gap: 2px; }
  .side { width: 340px; flex: none; }
  .out { flex: 1; overflow: hidden; min-width: 0; }
  .p-label { color: #a58a6b; font-size: 11px; letter-spacing: 0.14em; margin-bottom: 8px; }
  .green { color: #8fbf6a; }
  .amber { color: #d9a83a; }
  .mute { color: #a58a6b; }
  .fg { color: #e6d8c8; }
  .peach { color: #e9b98a; }
  .rust { color: #d97742; }
  .rm { color: #8a6a4c; }
  .nest { padding-left: 14px; }
  .ind { padding-left: 72px; }
  .side-foot { margin-top: auto; padding-top: 10px; border-top: 1px solid #382513; color: #a58a6b; font-size: 11px; }
  .cursor { display: inline-block; width: 8px; height: 14px; background: #d97742; vertical-align: text-bottom; margin-left: 4px; animation: tui-blink 1s step-end infinite; }
  @keyframes tui-blink { 50% { opacity: 0; } }
  .statusbar { display: flex; gap: 14px; color: #a58a6b; font-size: 12px; flex: none; }
  .mode { color: #171009; background: #d97742; padding: 0 8px; font-weight: 600; }
  .push { margin-left: auto; }
</style>
