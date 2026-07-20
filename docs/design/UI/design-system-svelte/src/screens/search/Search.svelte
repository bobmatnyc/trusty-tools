<script>
  import AppShell from '../../lib/AppShell.svelte';
  import Badge from '../../lib/Badge.svelte';
  import Button from '../../lib/Button.svelte';
  import RobotMark from '../../lib/RobotMark.svelte';
  import { searchNav } from './data.js';

  let query = $state('fn authenticate');
  let limit = $state('10');
  const results = [
    {
      file: 'src/auth/service.rs', symbol: 'authenticate', index: 'TRUSTY-TOOLS', lines: 'L42–88', score: '0.913',
      code: 'pub async fn authenticate(&self, token: &str) -> Result<Session> {\n    let claims = self.verify_jwt(token)?;\n    self.sessions.get_or_create(claims.sub).await\n}'
    },
    {
      file: 'src/auth/middleware.rs', symbol: null, index: 'TRUSTY-TOOLS', lines: 'L12–31', score: '0.847',
      code: 'let session = state.auth.authenticate(bearer.token()).await\n    .map_err(|_| StatusCode::UNAUTHORIZED)?;'
    }
  ];
</script>

{#snippet actions()}
  <Button variant="danger" size="sm">STOP</Button>
  <Button size="sm">RESTART</Button>
{/snippet}

<AppShell sidebar={searchNav('Search')} crumb="SEARCH" topbarActions={actions}>
  <h1 class="page-title">SEARCH</h1>
  <div class="card query-card">
    <div class="row">
      <input class="input" bind:value={query}>
      <input class="input limit" bind:value={limit}>
      <Button variant="primary">SEARCH</Button>
    </div>
    <div class="meta">
      <span class="hint">Searches 4 indexes</span>
      <span class="ms">· 8ms</span>
      <Badge tone="info">DEFINITION</Badge>
    </div>
  </div>

  <div class="results">
    {#each results as r}
      <div class="card result">
        <div class="head">
          <div class="ids">
            <span class="file">{r.file}</span>
            {#if r.symbol}<Badge tone="muted">{r.symbol}</Badge>{/if}
            <Badge tone="info">{r.index}</Badge>
            <span class="lines">{r.lines}</span>
          </div>
          <div class="score"><span class="score-label">SCORE</span><span class="score-val">{r.score}</span></div>
        </div>
        <pre>{r.code}</pre>
      </div>
    {/each}
  </div>

  <div class="chat">
    <div class="chat-head">
      <h2>CHAT</h2>
      <div class="index-pick">
        <span class="form-label inline-label">INDEX</span>
        <select class="select"><option>trusty-tools</option></select>
      </div>
      <Button size="sm">CLEAR</Button>
    </div>
    <div class="thread">
      <div class="msg user"><div class="bubble">Where is the session store created?</div></div>
      <div class="msg bot">
        <RobotMark size={26} body="#2b1c12" face="#e9b98a" antenna={false} />
        <div class="bubble">
          The session store is created in <code class="ref">src/auth/service.rs</code> inside
          <code class="ref">AuthService::new</code>, backed by the SQLite pool.
          <div class="sources">▸ 2 SOURCES</div>
        </div>
      </div>
    </div>
    <div class="composer">
      <textarea class="textarea" rows="2" placeholder="Ask a question… (Enter to send, Shift+Enter for newline)"></textarea>
      <Button variant="primary">SEND</Button>
    </div>
  </div>
</AppShell>

<style>
  h1 { font-size: 28px; margin: 0 0 24px; }
  .query-card { padding: 20px; margin-bottom: 16px; }
  .row { display: flex; gap: 8px; align-items: stretch; }
  .limit { width: 72px; flex: none; text-align: center; }
  .meta { display: flex; gap: 12px; align-items: center; margin-top: 12px; }
  .hint { color: var(--trusty-text-muted); font-size: 13px; }
  .ms { font: 500 11px var(--trusty-mono); color: var(--trusty-text-muted); }
  .results { display: flex; flex-direction: column; gap: 12px; margin-bottom: 20px; }
  .result { padding: 16px 20px; }
  .head { display: flex; justify-content: space-between; align-items: center; margin-bottom: 10px; gap: 12px; }
  .ids { display: flex; align-items: center; gap: 8px; min-width: 0; flex: 1; }
  .file { font: 600 13px var(--trusty-mono); color: var(--trusty-accent); }
  .lines { font: 400 11px var(--trusty-mono); color: var(--trusty-text-muted); }
  .score { display: flex; align-items: baseline; gap: 6px; flex: none; }
  .score-label { font: 600 9px var(--trusty-mono); letter-spacing: 0.14em; color: var(--trusty-text-muted); }
  .score-val { font: 600 14px var(--trusty-mono); color: var(--trusty-accent); }
  pre { margin: 0; padding: 12px 14px; background: var(--trusty-content-bg); border: 1px solid var(--trusty-surface-raised); border-radius: 4px; white-space: pre-wrap; word-break: break-word; font: 400 12px var(--trusty-mono); color: var(--trusty-text-secondary); line-height: 1.55; }
  .chat { border-top: 2px solid var(--trusty-border); padding-top: 20px; display: flex; flex-direction: column; gap: 12px; flex: 1; min-height: 0; }
  .chat-head { display: flex; align-items: center; gap: 16px; }
  .chat-head h2 { font: 700 16px var(--trusty-display); margin: 0; letter-spacing: 0.04em; }
  .index-pick { display: flex; align-items: center; gap: 10px; flex: 1; }
  .inline-label { margin: 0; }
  .index-pick .select { min-width: 170px; width: auto; padding: 5px 12px; font-size: 12px; }
  .thread { background: var(--trusty-surface-raised); border: 1.5px solid var(--trusty-border); border-radius: var(--trusty-radius); padding: 14px 16px; display: flex; flex-direction: column; gap: 12px; flex: 1; min-height: 120px; }
  .msg { display: flex; }
  .msg.user { justify-content: flex-end; }
  .msg.bot { justify-content: flex-start; gap: 10px; }
  .msg.bot :global(.robot) { margin-top: 2px; }
  .bubble { max-width: 72%; padding: 11px 15px; border-radius: 8px; line-height: 1.55; font-size: 13px; }
  .user .bubble { border-bottom-right-radius: 2px; background: var(--trusty-accent); color: #fff; }
  .bot .bubble { border-bottom-left-radius: 2px; background: var(--trusty-card-bg); border: 1.5px solid var(--trusty-border); }
  .ref { font: 500 12px var(--trusty-mono); color: var(--trusty-accent); }
  .sources { margin-top: 8px; padding-top: 8px; border-top: 1px solid var(--trusty-surface-raised); font: 600 10px var(--trusty-mono); letter-spacing: 0.1em; color: var(--trusty-text-muted); }
  .composer { display: flex; gap: 8px; align-items: flex-end; }
  .composer .textarea { min-height: 0; font-size: 13px; }
</style>
