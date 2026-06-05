<script>
  /*
   * Why: Operator-facing search box that fans out across every registered
   * index via the global `POST /search` endpoint, plus a conversational
   * chat panel (gated on `getChatAvailable()`) that lets operators ask
   * follow-up questions answered by the daemon's `/chat` endpoint.
   * What: Query input + results list + chat panel. Results render file
   * path, snippet, and relevance score. Chat renders a scrolling transcript
   * with user/assistant bubbles and collapsible source lists. Chat input is
   * disabled with a hint when no OpenRouter or local-model provider is
   * configured.
   * Test: With at least one index seeded, type "fn" and click Search;
   * results render with non-zero scores. With OPENROUTER_API_KEY set, the
   * chat panel's textarea is enabled; type a question and click Send; an
   * assistant bubble appears after the loading dots resolve.
   */
  import { api } from '../api.js';
  import { getIndexes, getChatAvailable } from '../state.svelte.js';
  import { tick } from 'svelte';

  // ─── Search state ────────────────────────────────────────────────────────────
  let query = $state('');
  let topK = $state(10);
  let results = $state([]);
  let intent = $state(null);
  let latencyMs = $state(null);
  let loading = $state(false);
  let error = $state(null);

  // ─── Shared state ────────────────────────────────────────────────────────────
  let indexes = $derived(getIndexes());
  let chatAvailable = $derived(getChatAvailable());

  // ─── Chat state ──────────────────────────────────────────────────────────────

  // Which index the chat should query; default to the first available.
  let chatIndexId = $state('');
  $effect(() => {
    if (!chatIndexId && indexes.length > 0) {
      chatIndexId = indexes[0].id;
    }
  });

  /** @type {Array<{role:'user'|'assistant', content:string, sources?:Array, error?:boolean}>} */
  let messages = $state([]);
  let chatInput = $state('');
  let chatLoading = $state(false);
  let transcriptEl = $state(null);
  // Per-message collapsed state for source lists (keyed by message index).
  let sourcesOpen = $state({});

  async function runSearch() {
    if (!query.trim()) return;
    loading = true;
    error = null;
    try {
      const body = await api.globalSearch(query.trim(), topK, false);
      results = body.results || [];
      intent = body.intent ?? null;
      latencyMs = body.latency_ms ?? null;
    } catch (e) {
      error = e.message || String(e);
      results = [];
    } finally {
      loading = false;
    }
  }

  function onSearchKey(e) {
    if (e.key === 'Enter') runSearch();
  }

  /**
   * Why: The daemon returns either `compact_snippet` (default) or full
   * `content` (when full_content=true). Pick whichever is present and trim.
   * What: Returns a short snippet for display.
   * Test: Pass `{compact_snippet: 'abc'}`, expect 'abc'.
   */
  function snippet(chunk) {
    const raw = chunk.compact_snippet || chunk.content || '';
    if (raw.length <= 320) return raw;
    return raw.slice(0, 320) + '…';
  }

  /**
   * Why: Auto-scrolling keeps the most-recent message visible without the
   * user needing to manually scroll after every exchange.
   * What: Scrolls `transcriptEl` to its `scrollHeight` after Svelte has
   * flushed DOM updates for the new message (via `tick()`).
   * Test: After `messages.push(...)`, call `scrollToBottom()`, assert
   * `el.scrollTop === el.scrollHeight - el.clientHeight`.
   */
  async function scrollToBottom() {
    await tick();
    if (transcriptEl) {
      transcriptEl.scrollTop = transcriptEl.scrollHeight;
    }
  }

  /**
   * Why: Separates pre-send validation from the async network call so errors
   * are surfaced cleanly in the UI without a blank loading state.
   * What: Validates input, appends user bubble, calls POST /chat with the
   * selected index and existing history, then appends assistant bubble (or
   * an error bubble on failure). A 503 gets a friendly "configure OpenRouter"
   * message instead of a raw HTTP error string.
   * Test: With a seeded index and OPENROUTER_API_KEY set, call sendMessage()
   * with a non-empty input and assert `messages` contains both user and
   * assistant entries. Without a key the send button is disabled so this path
   * is unreachable via normal UI interaction.
   */
  async function sendMessage() {
    const text = chatInput.trim();
    if (!text || chatLoading || !chatAvailable) return;
    if (!chatIndexId) return;

    chatInput = '';
    chatLoading = true;

    // Append user message immediately so the UI feels responsive.
    messages = [...messages, { role: 'user', content: text }];
    await scrollToBottom();

    // Build history for the request: everything except the user message we
    // just appended, keeping only role/content fields. Cap to the last
    // HISTORY_PAIRS exchanges (issue #781) so the /chat payload stays bounded.
    const HISTORY_PAIRS = 10;
    const history = messages
      .slice(0, -1)
      .filter((m) => m.role === 'user' || m.role === 'assistant')
      .map((m) => ({ role: m.role, content: m.content }))
      .slice(-HISTORY_PAIRS * 2);

    try {
      const body = await api.chat(chatIndexId, text, history);
      const reply = body.reply || body.answer || '(no reply)';
      const sources = body.sources || [];
      messages = [...messages, { role: 'assistant', content: reply, sources }];
    } catch (e) {
      // Surface 503 "no provider" as a friendly hint; expose other errors verbatim.
      // Use e.status (numeric) from ApiError rather than substring-matching the
      // message string (issue #781 — structured 503 detection).
      const detail = e.message || String(e);
      const friendly =
        e.status === 503
          ? 'Chat unavailable — set OPENROUTER_API_KEY in .env.local and restart the daemon.'
          : `Request failed: ${detail}`;
      messages = [
        ...messages,
        { role: 'assistant', content: friendly, error: true }
      ];
    } finally {
      chatLoading = false;
      await scrollToBottom();
    }
  }

  function onChatKey(e) {
    // Send on Enter; allow Shift+Enter for multi-line.
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      sendMessage();
    }
  }

  /**
   * Why: Operators may want to start a fresh conversation after switching
   * indexes or topics without reloading the page.
   * What: Clears the messages array and resets the chat input field.
   * Test: Add messages, call clearChat(), assert messages.length === 0.
   */
  function clearChat() {
    messages = [];
    chatInput = '';
    sourcesOpen = {};
  }

  function toggleSources(idx) {
    sourcesOpen = { ...sourcesOpen, [idx]: !sourcesOpen[idx] };
  }
</script>

<h1 class="page-title">Search</h1>

<!-- ─── Search panel ─────────────────────────────────────────────────────── -->
<div class="card mb-4">
  <div class="card-body">
    <div class="search-row">
      <input
        type="text"
        class="input"
        placeholder="Search across all indexes…"
        bind:value={query}
        onkeydown={onSearchKey}
      />
      <input
        type="number"
        class="input top-k"
        min="1"
        max="100"
        bind:value={topK}
        title="top_k"
      />
      <button
        class="btn btn-primary"
        onclick={runSearch}
        disabled={loading || !query.trim()}
      >
        {loading ? 'Searching…' : 'Search'}
      </button>
    </div>
    <div class="meta">
      {#if indexes.length === 0}
        <span class="text-muted text-sm"
          >No indexes registered — create one from the Indexes view.</span
        >
      {:else}
        <span class="text-muted text-sm">
          Searches {indexes.length} index{indexes.length === 1 ? '' : 'es'}.
        </span>
      {/if}
      {#if latencyMs !== null}
        <span class="text-muted text-sm">· {latencyMs}ms</span>
      {/if}
      {#if intent}
        <span class="badge badge-info">{intent}</span>
      {/if}
    </div>
  </div>
</div>

{#if error}
  <div class="card" style="border-color: var(--trusty-danger)">
    <div class="card-body" style="color: var(--trusty-danger)">{error}</div>
  </div>
{/if}

{#if results.length === 0 && !loading && !error}
  <div class="empty">
    {#if query.trim()}
      No results.
    {:else}
      Type a query above to search across all registered indexes.
    {/if}
  </div>
{:else}
  <div class="results">
    {#each results as r, i (r.id || i)}
      <div class="result">
        <div class="result-head">
          <div class="result-path">
            <span class="text-mono text-sm">{r.file || r.path || r.id}</span>
            {#if r.function}
              <span class="badge badge-muted">{r.function}</span>
            {/if}
            {#if r.index_id}
              <span class="badge badge-info">{r.index_id}</span>
            {/if}
            {#if r.match_reason}
              <span class="badge">{r.match_reason}</span>
            {/if}
            {#if r.start_line}
              <span class="text-muted text-xs">L{r.start_line}{r.end_line ? `–${r.end_line}` : ''}</span>
            {/if}
          </div>
          <div class="result-score">
            <span class="score-label">score</span>
            <span class="score-value">{(r.score ?? 0).toFixed(3)}</span>
          </div>
        </div>
        <pre class="snippet">{snippet(r)}</pre>
      </div>
    {/each}
  </div>
{/if}

<!-- ─── Chat panel ───────────────────────────────────────────────────────── -->
<div class="chat-section">
  <div class="chat-header">
    <h2 class="section-title">Chat</h2>
    {#if !chatAvailable}
      <span class="chat-unavailable-hint text-muted text-sm">
        Set <code>OPENROUTER_API_KEY</code> in <code>.env.local</code> to enable chat.
      </span>
    {:else}
      <div class="chat-toolbar">
        {#if indexes.length > 0}
          <label class="toolbar-label" for="chat-index-picker">Index</label>
          <select id="chat-index-picker" class="input select-sm" bind:value={chatIndexId}>
            {#each indexes as ix}
              <option value={ix.id}>{ix.id}</option>
            {/each}
          </select>
        {/if}
        {#if messages.length > 0}
          <button class="btn btn-sm" onclick={clearChat} title="Clear conversation">
            Clear
          </button>
        {/if}
      </div>
    {/if}
  </div>

  {#if chatAvailable}
    <!-- Transcript -->
    <div class="transcript" bind:this={transcriptEl}>
      {#if messages.length === 0}
        <div class="chat-empty">
          {#if indexes.length === 0}
            <span class="text-muted text-sm">Create an index first, then ask a question.</span>
          {:else}
            <span class="text-muted text-sm">Ask a question about your codebase below.</span>
          {/if}
        </div>
      {:else}
        {#each messages as msg, i}
          <div class="bubble-row" class:user-row={msg.role === 'user'}>
            <div
              class="bubble"
              class:bubble-user={msg.role === 'user'}
              class:bubble-assistant={msg.role === 'assistant'}
              class:bubble-error={msg.error}
            >
              <p class="bubble-content">{msg.content}</p>
              {#if msg.sources && msg.sources.length > 0}
                <div class="sources-block">
                  <button
                    class="sources-toggle"
                    onclick={() => toggleSources(i)}
                    type="button"
                  >
                    {sourcesOpen[i] ? '▾' : '▸'}
                    {msg.sources.length} source{msg.sources.length === 1 ? '' : 's'}
                  </button>
                  {#if sourcesOpen[i]}
                    <ul class="sources-list">
                      {#each msg.sources as src}
                        <li class="source-item">
                          <span class="text-mono text-xs">{src.file}</span>
                          <span class="text-muted text-xs">
                            L{src.start_line}–{src.end_line}
                          </span>
                          {#if src.match_reason}
                            <span class="badge badge-muted">{src.match_reason}</span>
                          {/if}
                        </li>
                      {/each}
                    </ul>
                  {/if}
                </div>
              {/if}
            </div>
          </div>
        {/each}
        {#if chatLoading}
          <div class="bubble-row">
            <div class="bubble bubble-assistant thinking">
              <span class="dot"></span><span class="dot"></span><span class="dot"></span>
            </div>
          </div>
        {/if}
      {/if}
    </div>

    <!-- Input bar -->
    <div class="chat-input-bar">
      <textarea
        class="chat-textarea"
        placeholder={indexes.length === 0
          ? 'Create an index first…'
          : 'Ask a question… (Enter to send, Shift+Enter for newline)'}
        rows="2"
        bind:value={chatInput}
        onkeydown={onChatKey}
        disabled={chatLoading || indexes.length === 0}
      ></textarea>
      <button
        class="btn btn-primary send-btn"
        onclick={sendMessage}
        disabled={chatLoading || !chatInput.trim() || indexes.length === 0}
      >
        {chatLoading ? '…' : 'Send'}
      </button>
    </div>
  {:else}
    <!-- Disabled state: chat panel shown but locked, with clear explanation -->
    <div class="chat-disabled-panel">
      <textarea
        class="chat-textarea chat-textarea-disabled"
        placeholder="Chat disabled — configure OPENROUTER_API_KEY to enable"
        rows="2"
        disabled
      ></textarea>
      <button class="btn btn-primary send-btn" disabled>Send</button>
    </div>
  {/if}
</div>

<style>
  .page-title {
    font-size: var(--trusty-fs-xl);
    margin: 0 0 var(--trusty-space-5) 0;
    font-weight: 600;
  }
  .search-row {
    display: flex;
    gap: var(--trusty-space-2);
    align-items: stretch;
  }
  .top-k {
    width: 80px;
    flex: 0 0 80px;
  }
  .meta {
    display: flex;
    gap: var(--trusty-space-3);
    align-items: center;
    margin-top: var(--trusty-space-3);
  }
  .results {
    display: flex;
    flex-direction: column;
    gap: var(--trusty-space-3);
  }
  .result {
    background: var(--trusty-card-bg);
    border: 1px solid var(--trusty-border);
    border-radius: var(--trusty-radius);
    padding: var(--trusty-space-4);
  }
  .result-head {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: var(--trusty-space-3);
    gap: var(--trusty-space-3);
  }
  .result-path {
    display: flex;
    align-items: center;
    gap: var(--trusty-space-2);
    min-width: 0;
    flex: 1;
  }
  .result-score {
    display: flex;
    flex-direction: column;
    align-items: flex-end;
    flex-shrink: 0;
  }
  .score-label {
    font-size: var(--trusty-fs-xs);
    color: var(--trusty-text-muted);
    text-transform: uppercase;
    letter-spacing: 0.06em;
  }
  .score-value {
    font-family: var(--trusty-mono);
    font-weight: 600;
    color: var(--trusty-text-primary);
  }
  .snippet {
    margin: 0;
    padding: var(--trusty-space-3);
    background: var(--trusty-content-bg);
    border-radius: var(--trusty-radius);
    overflow-x: auto;
    white-space: pre-wrap;
    word-break: break-word;
    font-size: var(--trusty-fs-xs);
    color: var(--trusty-text-secondary);
    line-height: 1.5;
  }

  /* ─── Chat panel ─────────────────────────────────────────────────────── */
  .chat-section {
    margin-top: var(--trusty-space-6);
    border-top: 1px solid var(--trusty-border);
    padding-top: var(--trusty-space-5);
    display: flex;
    flex-direction: column;
    gap: var(--trusty-space-3);
  }

  .chat-header {
    display: flex;
    align-items: center;
    gap: var(--trusty-space-4);
    flex-wrap: wrap;
  }

  .section-title {
    font-size: var(--trusty-fs-lg);
    font-weight: 600;
    margin: 0;
    flex-shrink: 0;
  }

  .chat-unavailable-hint {
    font-size: var(--trusty-fs-sm);
  }

  .chat-toolbar {
    display: flex;
    align-items: center;
    gap: var(--trusty-space-3);
    flex: 1;
  }

  .toolbar-label {
    font-size: var(--trusty-fs-sm);
    font-weight: 500;
    color: var(--trusty-text-secondary);
    white-space: nowrap;
  }

  .select-sm {
    min-width: 160px;
    max-width: 280px;
    padding-top: 4px;
    padding-bottom: 4px;
    font-size: var(--trusty-fs-sm);
  }

  /* Transcript */
  .transcript {
    background: var(--trusty-content-bg);
    border: 1px solid var(--trusty-border);
    border-radius: var(--trusty-radius);
    min-height: 140px;
    max-height: 420px;
    overflow-y: auto;
    padding: var(--trusty-space-3) var(--trusty-space-4);
    display: flex;
    flex-direction: column;
    gap: var(--trusty-space-3);
  }

  .chat-empty {
    padding: var(--trusty-space-4);
    text-align: center;
  }

  /* Bubbles */
  .bubble-row {
    display: flex;
    justify-content: flex-start;
  }
  .user-row {
    justify-content: flex-end;
  }

  .bubble {
    max-width: 78%;
    padding: var(--trusty-space-3) var(--trusty-space-4);
    border-radius: var(--trusty-radius-lg);
    line-height: 1.6;
    word-break: break-word;
  }

  .bubble-user {
    background: var(--trusty-accent);
    color: #fff;
    border-bottom-right-radius: var(--trusty-radius-sm);
  }

  .bubble-assistant {
    background: var(--trusty-card-bg);
    border: 1px solid var(--trusty-border);
    color: var(--trusty-text-primary);
    border-bottom-left-radius: var(--trusty-radius-sm);
  }

  .bubble-error {
    background: var(--trusty-danger-soft, #fff0f0);
    border-color: var(--trusty-danger, #c0392b);
    color: var(--trusty-danger, #c0392b);
  }

  .bubble-content {
    margin: 0;
    white-space: pre-wrap;
    font-size: var(--trusty-fs-sm);
  }

  /* Thinking dots animation */
  .thinking {
    display: flex;
    align-items: center;
    gap: 5px;
    padding: var(--trusty-space-3) var(--trusty-space-4);
    min-width: 56px;
  }
  .dot {
    width: 7px;
    height: 7px;
    background: var(--trusty-text-muted);
    border-radius: 50%;
    animation: blink 1.4s infinite both;
  }
  .dot:nth-child(2) { animation-delay: 0.2s; }
  .dot:nth-child(3) { animation-delay: 0.4s; }
  @keyframes blink {
    0%, 80%, 100% { opacity: 0.2; transform: scale(0.8); }
    40% { opacity: 1; transform: scale(1); }
  }

  /* Sources */
  .sources-block {
    margin-top: var(--trusty-space-2);
    border-top: 1px solid var(--trusty-border);
    padding-top: var(--trusty-space-2);
  }
  .sources-toggle {
    background: none;
    border: none;
    cursor: pointer;
    font-size: var(--trusty-fs-xs);
    color: var(--trusty-text-muted);
    padding: 0;
    display: flex;
    align-items: center;
    gap: 4px;
  }
  .sources-toggle:hover {
    color: var(--trusty-text-secondary);
  }
  .sources-list {
    list-style: none;
    margin: var(--trusty-space-2) 0 0 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--trusty-space-1);
  }
  .source-item {
    display: flex;
    align-items: center;
    gap: var(--trusty-space-2);
    padding: 2px 0;
  }

  /* Input bar */
  .chat-input-bar,
  .chat-disabled-panel {
    display: flex;
    gap: var(--trusty-space-2);
    align-items: flex-end;
  }

  .chat-textarea {
    flex: 1;
    resize: none;
    border: 1px solid var(--trusty-border);
    border-radius: var(--trusty-radius);
    padding: var(--trusty-space-2) var(--trusty-space-3);
    font-family: var(--trusty-font);
    font-size: var(--trusty-fs-sm);
    color: var(--trusty-text-primary);
    background: var(--trusty-card-bg);
    outline: none;
    line-height: 1.5;
  }
  .chat-textarea:focus {
    border-color: var(--trusty-accent);
    box-shadow: 0 0 0 2px var(--trusty-accent-soft, rgba(99,102,241,0.15));
  }
  .chat-textarea:disabled,
  .chat-textarea-disabled {
    background: var(--trusty-content-bg);
    color: var(--trusty-text-muted);
    cursor: not-allowed;
  }
  .send-btn {
    flex-shrink: 0;
    min-width: 68px;
  }
</style>
