<!--
  The fullscreen machine-status screensaver (#6519, phase 3 of #6516).

  Why: phase 4 (#6520) points a macOS `.saver` WKWebView at
  `http://127.0.0.1:7788/ui/screensaver`. That view gets no interaction, no
  chrome, and hours of uptime, so this screen renders correctly on mount and
  keeps rendering through a daemon restart. It reuses the phase-2 DATA layer
  (`machineStatus.js`) but not `MachineStatusPanel`'s markup, which is laid out
  for a ~1100px column and is unreadable across a room.
  What: a renderer over `screensaver.js` and `machineStatus.js`. Two frames
  rotate on a 20s cycle — host cards + service counts, then the full service
  table — so one 15s poll always lands between frames. Three timers (poll,
  clock, rotation) are the only moving parts and all three are cleared on
  destroy. The layout is correct WITHOUT the Fullscreen API, because the
  WKWebView is already fullscreen and need not expose it.
  Test: `screensaver.test.js` covers the rotation, idle, backoff and routing
  decisions; the rendered screen is verified by the binary smoke run and a
  1920×1080 Playwright screenshot of `/ui/screensaver`.
-->
<script>
  import { onMount, onDestroy } from 'svelte';
  import Badge from './Badge.svelte';
  import BrandLockup from './BrandLockup.svelte';
  import StatCard from './StatCard.svelte';
  import { formatLastUsed } from './lastUsed.js';
  import {
    fetchMachineStatus,
    pressureTone,
    rollupTone,
    serviceRows,
    statCards,
    toneLabel,
  } from './machineStatus.js';
  import {
    CONSOLE_URL,
    POLL_BASE_MS,
    ROTATE_MS,
    enteredFromIdle,
    nextPollDelayMs,
    rotationIndexAt,
  } from './screensaver.js';

  /** Frame 0 is the host cards, frame 1 the service table. */
  const FRAMES = [0, 1];
  const FRAME_COUNT = FRAMES.length;
  const CLOCK_MS = 1000;
  /** How long the fullscreen hint stays up before fading out. */
  const HINT_MS = 6000;

  // Last good payload, kept across failures — an unattended screen showing
  // stale-but-labelled numbers beats one showing an error box.
  let status = $state(null);
  let lastOkUnix = $state(null);
  let loading = $state(true);
  let elapsedMs = $state(0);
  let now = $state(new Date());
  let showHint = $state(false);

  let consecutiveFailures = 0;
  let startedAt = 0;
  let pollTimer;
  let clockTimer;
  let rotateTimer;
  let hintTimer;
  let armed = false;
  let fromIdle = false;

  /**
   * Read the status once and schedule the next read.
   *
   * Self-rescheduling rather than a fixed interval because the delay itself is
   * the backoff: `nextPollDelayMs` widens the gap once the daemon has missed
   * three answers in a row, and a `setInterval` cannot change its own period.
   * A failure leaves `status` untouched, so the last good snapshot stays up.
   */
  async function poll() {
    const result = await fetchMachineStatus();
    if (result.status) {
      status = result.status;
      lastOkUnix = Math.floor(Date.now() / 1000);
      consecutiveFailures = 0;
    } else {
      consecutiveFailures += 1;
    }
    loading = false;
    pollTimer = setTimeout(poll, nextPollDelayMs(consecutiveFailures, POLL_BASE_MS));
  }

  /**
   * Hand the console back.
   *
   * A full navigation, not a component swap: `main.js` chooses the mounted
   * component from the pathname, so the pathname is the state.
   */
  function exitToConsole() {
    if (document.fullscreenElement) document.exitFullscreen?.().catch(() => {});
    window.location.assign(CONSOLE_URL);
  }

  /**
   * What a keypress or click does, which depends on how this screen was reached.
   *
   * Reached by the idle timer, someone is trying to get their console back and
   * the first input does exactly that. Opened deliberately, the first input is
   * instead spent on `requestFullscreen()` — the API is only granted inside a
   * user gesture, so there is no other moment to ask — and the second input
   * leaves. A browser that refuses fullscreen is a no-op, never a dead end.
   */
  function handleInput() {
    if (fromIdle || armed) {
      exitToConsole();
      return;
    }
    armed = true;
    showHint = false;
    if (document.fullscreenEnabled && !document.fullscreenElement) {
      document.documentElement.requestFullscreen?.().catch(() => {});
    }
  }

  onMount(() => {
    // The screensaver is dark regardless of the operator's console theme: this
    // is a dim room at 2am, and the theme store is deliberately bypassed so the
    // choice never leaks back into `localStorage`.
    document.documentElement.setAttribute('data-theme', 'dark');
    fromIdle = enteredFromIdle(window.location.search);

    startedAt = Date.now();
    poll();
    clockTimer = setInterval(() => (now = new Date()), CLOCK_MS);
    rotateTimer = setInterval(() => (elapsedMs = Date.now() - startedAt), ROTATE_MS);

    if (!fromIdle && document.fullscreenEnabled) {
      showHint = true;
      hintTimer = setTimeout(() => {
        if (!document.fullscreenElement) showHint = false;
      }, HINT_MS);
    }
  });

  onDestroy(() => {
    clearTimeout(pollTimer);
    clearTimeout(hintTimer);
    clearInterval(clockTimer);
    clearInterval(rotateTimer);
  });

  let frame = $derived(rotationIndexAt(elapsedMs, ROTATE_MS, FRAME_COUNT));
  let cards = $derived(statCards(status?.host));
  let rollup = $derived(status?.services ?? null);
  let nowUnix = $derived(Math.floor(now.getTime() / 1000));
  // Same reason as `freshness` below: read the ticking clock, not the default,
  // so the Collected column keeps ageing while the daemon is down.
  let rows = $derived(serviceRows(status, nowUnix));
  let clock = $derived(now.toLocaleTimeString());
  let day = $derived(now.toLocaleDateString(undefined, { weekday: 'long', month: 'long', day: 'numeric' }));
  // Only ever shown once a poll has succeeded; before that there is nothing to
  // be stale about and the loading line covers it.
  //
  // The clock is passed in rather than left to `formatLastUsed`'s default,
  // because that default is read at call time and a `$derived` only recomputes
  // when a dependency changes. Depending on `lastOkUnix` alone froze this line
  // at "just now" for as long as the daemon stayed down — which is precisely
  // when it is the only thing on screen saying the numbers are old.
  let freshness = $derived(
    lastOkUnix === null
      ? null
      : `updated ${formatLastUsed({ last_used_unix: lastOkUnix }, nowUnix)}`,
  );
</script>

<svelte:window onkeydown={handleInput} onpointerdown={handleInput} />

<div class="foundry saver">
  <header class="saver-top">
    <div class="brand"><BrandLockup /></div>
    <div class="time">
      <div class="clock">{clock}</div>
      <div class="day">{day}</div>
    </div>
  </header>

  {#if loading}
    <div class="middle"><p class="waiting">Sampling host metrics…</p></div>
  {:else if frame === 0}
    <div class="middle">
      <div class="host-grid">
        {#each cards as card (card.key)}
          <StatCard label={card.label} value={card.value} meta={card.meta}>
            {#if card.badge}
              <div class="card-badge"><Badge tone={card.tone}>{card.badge}</Badge></div>
            {/if}
            {#if card.extra}<div class="card-extra">{card.extra}</div>{/if}
          </StatCard>
        {/each}
      </div>
      <div class="counts">
        <span class="count"><b>{rollup?.total ?? 0}</b> services</span>
        <span class="count ok"><b>{rollup?.ok ?? 0}</b> ok</span>
        <span class="count degraded"><b>{rollup?.degraded ?? 0}</b> degraded</span>
        <span class="count error"><b>{rollup?.error ?? 0}</b> error</span>
      </div>
    </div>
  {:else}
    <div class="middle">
      <div class="services-head">
        <h2>Services</h2>
        {#if rollup}<Badge tone={rollupTone(rollup)}>{rollup.total} reporting</Badge>{/if}
      </div>
      {#if rows.length > 0}
        <table class="table saver-table">
          <thead>
            <tr><th>Service</th><th>Version</th><th>Status</th><th>Collected</th></tr>
          </thead>
          <tbody>
            {#each rows as row (row.id)}
              <tr>
                <td class="name">{row.displayName}</td>
                <td class="mono">{row.version}</td>
                <td><Badge tone={row.tone}>{row.healthLabel}</Badge></td>
                <td class="mono muted">{row.collected}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      {:else}
        <p class="waiting">No service has reported metrics yet.</p>
      {/if}
    </div>
  {/if}

  <footer class="saver-bottom">
    <div class="left">
      {#if status?.host}
        <Badge tone={pressureTone(status.host.overall_pressure)} dot>
          {toneLabel(status.host.overall_pressure)}
        </Badge>
      {/if}
      {#if freshness}<span class="freshness">{freshness}</span>{/if}
    </div>
    <div class="dots" aria-hidden="true">
      {#each FRAMES as f (f)}
        <span class="dot" class:on={f === frame}></span>
      {/each}
    </div>
    {#if showHint}<span class="hint">click for fullscreen</span>{/if}
  </footer>
</div>

<style>
  /* The route owns the whole viewport: no scrollbars, no chrome, nothing that
     needs a pointer. Fixed rather than 100vh so a mobile URL bar cannot push
     the footer out of view. */
  .saver {
    position: fixed;
    inset: 0;
    width: 100vw;
    height: 100vh;
    overflow: hidden;
    display: flex;
    flex-direction: column;
    gap: clamp(1rem, 2.5vh, 2.5rem);
    padding: clamp(1.5rem, 4vh, 4rem) clamp(1.5rem, 4vw, 5rem);
    background: var(--trusty-content-bg);
    color: var(--trusty-text-primary);
    cursor: none;
  }
  :global(body) { margin: 0; overflow: hidden; }

  .saver-top { display: flex; align-items: flex-start; justify-content: space-between; gap: 2rem; }
  /* BrandLockup sets its own type scale for a 1100px header; the screensaver is
     read from across a room, so the whole lockup is scaled up as a unit rather
     than restyled piecemeal. */
  .brand { transform: scale(1.7); transform-origin: left top; }
  .time { text-align: right; }
  .clock {
    font-family: var(--trusty-display);
    font-size: clamp(2.5rem, 6vw, 6rem);
    font-weight: 700;
    line-height: 1;
    letter-spacing: 0.02em;
    font-variant-numeric: tabular-nums;
  }
  .day {
    font-family: var(--trusty-mono);
    font-size: clamp(0.7rem, 1.1vw, 1.1rem);
    letter-spacing: 0.16em;
    text-transform: uppercase;
    color: var(--trusty-text-secondary);
    margin-top: 0.4rem;
  }

  .middle {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    justify-content: center;
    gap: clamp(1rem, 3vh, 3rem);
  }

  .host-grid {
    display: grid;
    grid-template-columns: repeat(4, 1fr);
    gap: clamp(0.75rem, 1.5vw, 2rem);
  }
  /* The four host cards are the point of the frame, so they take the viewport's
     scale rather than the dashboard's fixed rem sizes. */
  .host-grid :global(.stat) { padding: clamp(1rem, 2.5vw, 2.5rem); }
  .host-grid :global(.stat-label) { font-size: clamp(0.7rem, 1vw, 1.05rem); }
  .host-grid :global(.stat-value) {
    font-size: clamp(1.6rem, 3.4vw, 4rem);
    line-height: 1.1;
    overflow-wrap: anywhere;
  }
  .host-grid :global(.stat-meta) { font-size: clamp(0.75rem, 1.1vw, 1.2rem); }
  .host-grid :global(.badge) { font-size: clamp(0.7rem, 0.9vw, 1rem); padding: 4px 10px; }
  .card-badge { margin-top: 0.6rem; }
  .card-extra {
    margin-top: 0.5rem;
    font-family: var(--trusty-mono);
    font-size: clamp(0.7rem, 1vw, 1.05rem);
    color: var(--trusty-text-secondary);
  }

  .counts {
    display: flex;
    flex-wrap: wrap;
    gap: clamp(1.5rem, 4vw, 4rem);
    justify-content: center;
    font-family: var(--trusty-mono);
    font-size: clamp(0.8rem, 1.2vw, 1.3rem);
    letter-spacing: 0.14em;
    text-transform: uppercase;
    color: var(--trusty-text-secondary);
  }
  .count b {
    font-family: var(--trusty-display);
    font-size: clamp(1.5rem, 3vw, 3.5rem);
    color: var(--trusty-text-primary);
    margin-right: 0.5rem;
  }
  .count.ok b { color: var(--trusty-success); }
  .count.degraded b { color: var(--trusty-warning); }
  .count.error b { color: var(--trusty-danger); }

  .services-head { display: flex; align-items: center; gap: 1rem; }
  .services-head h2 {
    margin: 0;
    font-family: var(--trusty-display);
    font-size: clamp(1.2rem, 2.4vw, 2.6rem);
    font-weight: 700;
    letter-spacing: 0.06em;
    text-transform: uppercase;
  }
  /* Taller rows and viewport type: the phase-2 table is 0.875rem on 11px
     padding, which is a squint at 3 metres. */
  .saver-table { font-size: clamp(0.9rem, 1.5vw, 1.7rem); }
  .saver-table :global(th) { font-size: clamp(0.65rem, 0.85vw, 0.95rem); }
  .saver-table :global(th),
  .saver-table :global(td) { padding: clamp(0.6rem, 1.6vh, 1.6rem) clamp(0.75rem, 1.2vw, 1.6rem); }
  /* The stamp scales with the row it sits in — at the dashboard's fixed 10px it
     is a smudge beside 1.7rem row text. */
  .saver-table :global(.badge) { font-size: clamp(0.75rem, 1.1vw, 1.3rem); padding: 6px 14px; }
  .name { font-weight: 600; }
  .mono { font-family: var(--trusty-mono); }
  .muted { color: var(--trusty-text-muted); }

  .waiting {
    margin: 0;
    text-align: center;
    font-family: var(--trusty-mono);
    font-size: clamp(0.9rem, 1.4vw, 1.5rem);
    letter-spacing: 0.14em;
    text-transform: uppercase;
    color: var(--trusty-text-secondary);
  }

  .saver-bottom { display: flex; align-items: center; justify-content: space-between; gap: 1rem; }
  .left { display: flex; align-items: center; gap: 1rem; }
  .saver-bottom :global(.badge) { font-size: clamp(0.7rem, 0.9vw, 1rem); padding: 4px 10px; }
  /* Stale data stays on screen but says so — this is the only signal that the
     daemon stopped answering, and it must never become a modal. */
  .freshness {
    font-family: var(--trusty-mono);
    font-size: clamp(0.65rem, 0.9vw, 1rem);
    letter-spacing: 0.14em;
    text-transform: uppercase;
    color: var(--trusty-text-muted);
  }
  .dots { display: flex; gap: 0.5rem; }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--trusty-border);
  }
  .dot.on { background: var(--trusty-accent); }
  .hint {
    font-family: var(--trusty-mono);
    font-size: clamp(0.65rem, 0.9vw, 1rem);
    letter-spacing: 0.14em;
    text-transform: uppercase;
    color: var(--trusty-text-muted);
    animation: fade-in 0.6s ease-out;
  }
  @keyframes fade-in { from { opacity: 0; } to { opacity: 1; } }
</style>
