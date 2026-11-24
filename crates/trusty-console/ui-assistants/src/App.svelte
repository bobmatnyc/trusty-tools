<script>
  /*
   * Why: #11749, epic #11746 — one page to read an assistant's history and ask
   * it a question from any device on the tailnet, over the three
   * `/api/console/assistants…` routes. Asks can take tens of seconds, and the
   * answer is model output.
   * What: an instance list (the first instance is opened on load; an empty
   * list says "no assistant instances", as `ta list` does), that instance's
   * turns oldest first, with a note for skipped lines, a truncated read and a
   * shortened tail, and an ask box. One ask runs at a time: submit is disabled
   * and the box shows a pending state until it settles. A 200 appends the turn
   * (or shows it in a notice when no history is loaded, and reloads the
   * history after a reselection mid-ask); a failure shows `errorLines` (the 503 hint verbatim, the busy text on 429).
   * Every model and console string renders as text, never as HTML. A turns or
   * ask answer for an instance no longer selected is dropped.
   * Test: `src/App.test.js`.
   */
  import { onMount } from 'svelte';
  import { createApi, errorLines } from './lib/api.js';

  /** Turns asked for per instance; the console caps it at 200. */
  const TURN_LIMIT = 50;

  let { api = createApi() } = $props();

  let instances = $state([]);
  let listed = $state(false);
  let listError = $state(null);
  let selected = $state('');
  let history = $state(null);
  let historyError = $state(null);
  let question = $state('');
  let pending = $state('');
  let askError = $state(null);
  // An answer the history could not take (still loading, or failed to load).
  let askAnswer = $state(null);
  // Bumped on every selection; a turns answer under an older value is dropped.
  // Not reactive: no markup reads it.
  let ticket = 0;

  const canAsk = $derived(selected !== '' && pending === '' && question.trim() !== '');

  onMount(async () => {
    try {
      const answer = await api.list();
      instances = Array.isArray(answer.instances) ? answer.instances.map((i) => String(i.id)) : [];
      if (instances.length > 0) choose(instances[0]);
    } catch (err) {
      listError = errorLines(err);
    } finally {
      listed = true;
    }
  });

  async function choose(id) {
    ticket += 1;
    const mine = ticket;
    selected = id;
    history = null;
    historyError = null;
    askError = null;
    askAnswer = null;
    try {
      const answer = await api.turns(id, TURN_LIMIT);
      if (mine !== ticket) return;
      history = {
        id,
        turns: Array.isArray(answer.turns) ? answer.turns : [],
        total: Number(answer.total) || 0,
        skipped: Number(answer.skipped) || 0,
        truncated: answer.truncated === true,
      };
    } catch (err) {
      if (mine !== ticket) return;
      historyError = errorLines(err);
    }
  }

  async function ask(event) {
    event.preventDefault();
    if (!canAsk) return;
    const id = selected;
    const text = question;
    // #11749 review: a reselection while the ask runs reloads the history,
    // which may already hold this turn; appending it too would show it twice.
    const mine = ticket;
    pending = id;
    askError = null;
    askAnswer = null;
    try {
      const answer = await api.ask(id, text);
      if (selected !== id) return;
      question = '';
      const reply = String(answer.answer ?? '');
      if (mine !== ticket) {
        choose(id);
      } else if (history && history.id === id) {
        const turn = { at: new Date().toISOString(), question: text, answer: reply };
        history = { ...history, turns: [...history.turns, turn], total: history.total + 1 };
      } else {
        // #11749 review: no history to append to; never drop the answer.
        askAnswer = { question: text, answer: reply };
      }
    } catch (err) {
      if (selected === id) askError = errorLines(err);
    } finally {
      pending = '';
    }
  }

  const plural = (n, one, many) => `${n} ${n === 1 ? one : many}`;
</script>

<div class="shell">
  <header class="topbar">
    <h1 class="display">Trusty Assistants</h1>
    <p class="text-muted text-sm">Read an assistant's history and ask it a question. Answers come from the assistant's running ask daemon.</p>
  </header>

  <div class="body">
    <nav class="rail card" aria-label="Assistant instances">
      <div class="card-header"><span class="text-mono">Instances</span></div>
      <div class="rail-list">
        {#if listError}
          <div class="notice error" role="alert" data-testid="list-error">
            {#each listError as line}<p>{line}</p>{/each}
          </div>
        {:else if listed && instances.length === 0}
          <p class="empty text-mono" data-testid="no-instances">no assistant instances</p>
        {:else}
          <ul>
            {#each instances as id (id)}
              <li>
                <button
                  class="instance"
                  class:active={id === selected}
                  aria-current={id === selected ? 'true' : undefined}
                  data-testid="instance"
                  data-id={id}
                  onclick={() => choose(id)}>{id}</button
                >
              </li>
            {/each}
          </ul>
        {/if}
      </div>
    </nav>

    <main class="pane">
      {#if selected}
        <section class="card" aria-label="History">
          <div class="card-header">
            <span class="text-mono">History · {selected}</span>
            <span class="text-muted text-xs text-mono">oldest first</span>
          </div>
          <div class="card-body">
            {#if historyError}
              <div class="notice error" role="alert" data-testid="history-error">
                {#each historyError as line}<p>{line}</p>{/each}
              </div>
            {:else if !history}
              <p class="text-muted" data-testid="history-loading"><span class="spinner"></span> loading history</p>
            {:else}
              {#if history.truncated}
                <p class="notice warn" data-testid="truncated-note">showing the newest part of the history</p>
              {/if}
              {#if history.skipped > 0}
                <p class="notice warn" data-testid="skipped-note">
                  {plural(history.skipped, 'line', 'lines')} of the history could not be read
                </p>
              {/if}
              {#if history.total > history.turns.length}
                <p class="text-muted text-sm" data-testid="tail-note">
                  showing the last {history.turns.length} of {plural(history.total, 'turn', 'turns')}
                </p>
              {/if}
              {#if history.turns.length === 0}
                <p class="empty" data-testid="no-turns">No turns yet. Ask a question below.</p>
              {:else}
                <ol class="turns" data-testid="turns">
                  {#each history.turns as turn, i (i)}
                    <li class="turn" data-testid="turn">
                      <div class="turn-at text-mono text-xs text-muted">{turn.at}</div>
                      <div class="turn-q"><span class="who text-mono">Q</span><p class="text" data-testid="turn-question">{turn.question}</p></div>
                      <div class="turn-a"><span class="who text-mono">A</span><p class="text" data-testid="turn-answer">{turn.answer}</p></div>
                    </li>
                  {/each}
                </ol>
              {/if}
            {/if}
          </div>
        </section>

        <section class="card ask" aria-label="Ask">
          <form class="card-body" onsubmit={ask} data-testid="ask-form">
            <label class="form-label" for="question">Ask {selected}</label>
            <textarea
              id="question"
              class="textarea"
              data-testid="question"
              rows="3"
              placeholder="Type a question"
              bind:value={question}
            ></textarea>
            <div class="flex-between mt-3">
              <span class="text-muted text-xs" data-testid="ask-pending" aria-live="polite">
                {#if pending}<span class="spinner"></span> asking {pending}; this can take a while{/if}
              </span>
              <button class="btn btn-primary" type="submit" data-testid="ask" disabled={!canAsk}>
                {pending ? 'Asking…' : 'Ask'}
              </button>
            </div>
            {#if askAnswer}
              <div class="notice mt-3" role="status" data-testid="ask-answer">
                <p class="text-mono text-xs">Q</p>
                <p class="text">{askAnswer.question}</p>
                <p class="text-mono text-xs mt-3">A</p>
                <p class="text">{askAnswer.answer}</p>
              </div>
            {/if}
            {#if askError}
              <div class="notice error mt-3" role="alert" data-testid="ask-error">
                {#each askError as line}<p>{line}</p>{/each}
              </div>
            {/if}
          </form>
        </section>
      {:else if listed && instances.length > 0}
        <p class="empty">Choose an instance.</p>
      {/if}
    </main>
  </div>
</div>

<style>
  :global(body) {
    margin: 0;
  }
  .shell {
    max-width: 1200px;
    margin: 0 auto;
    padding: var(--trusty-space-5);
  }
  .topbar h1 {
    margin: 0 0 var(--trusty-space-1);
    font-family: var(--trusty-display);
    font-size: var(--trusty-fs-xl);
  }
  .topbar p {
    margin: 0;
  }
  .body {
    display: flex;
    gap: var(--trusty-space-5);
    margin-top: var(--trusty-space-5);
    align-items: flex-start;
  }
  .rail {
    flex: 0 0 var(--trusty-sidebar-width);
  }
  .rail-list ul {
    list-style: none;
    margin: 0;
    padding: var(--trusty-space-2);
  }
  .instance {
    display: block;
    width: 100%;
    text-align: left;
    padding: var(--trusty-space-2) var(--trusty-space-3);
    border: 1px solid transparent;
    border-radius: var(--trusty-radius-sm);
    background: transparent;
    color: var(--trusty-text-primary);
  }
  .instance:hover {
    background: var(--trusty-surface-hover);
  }
  .instance.active {
    border-color: var(--trusty-accent);
    color: var(--trusty-accent);
    background: var(--trusty-accent-soft);
  }
  .pane {
    flex: 1 1 auto;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: var(--trusty-space-5);
  }
  .turns {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  .turn {
    padding: var(--trusty-space-3) 0;
    border-bottom: 1px solid var(--trusty-border);
  }
  .turn:last-child {
    border-bottom: none;
  }
  .turn-q,
  .turn-a {
    display: flex;
    gap: var(--trusty-space-3);
    margin-top: var(--trusty-space-2);
  }
  .who {
    flex: 0 0 auto;
    color: var(--trusty-text-muted);
  }
  .turn-a .who {
    color: var(--trusty-accent);
  }
  .text {
    margin: 0;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .ask textarea {
    width: 100%;
  }
  .notice {
    margin: 0 0 var(--trusty-space-3);
    padding: var(--trusty-space-2) var(--trusty-space-3);
    border: 1px solid var(--trusty-border);
    border-left-width: 3px;
    border-radius: var(--trusty-radius-sm);
    font-size: var(--trusty-fs-sm);
  }
  .notice p {
    margin: 0;
    overflow-wrap: anywhere;
  }
  .notice.error {
    border-left-color: var(--trusty-danger);
    background: var(--trusty-danger-soft);
  }
  .notice.warn {
    border-left-color: var(--trusty-warning);
    background: var(--trusty-warning-soft);
  }
  /* A phone on the tailnet: stack the list above the pane. */
  @media (max-width: 720px) {
    .shell {
      padding: var(--trusty-space-3);
    }
    .body {
      flex-direction: column;
      align-items: stretch;
    }
    .rail {
      flex: 0 0 auto;
    }
  }
</style>
