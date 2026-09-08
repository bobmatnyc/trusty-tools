<!--
  The Disk view's detail panel (#6929, DOC-73 §16.3).

  Why it re-fetches nothing: the panel renders the SAME `ReclaimCandidate`-shaped
  row the sunburst was drawn from — §16.3 calls it "a direct render of the
  struct, not a new computation". A second request would show a row from a
  later survey than the arc that was clicked, so a worktree could be one tier in
  the chart and another in the panel. `GET /api/console/disk/worktrees/{id}`
  exists for a deep link or a script; this component never calls it.
  What: path, branch, tier, the reclaim gate and its reason string verbatim,
  every classification reason, bytes with the size note behind them, PR, and the
  claiming session. DISPLAY ONLY — there is no clear button here; #6930 owns it.
  Test: `diskSunburst.test.js` covers the tier mapping and the fold this panel
  reads; the markup itself is covered by the binary smoke run on #6929.
-->
<script>
  import Badge from './Badge.svelte';
  import { formatBytes, tierLabel, tierTone } from './diskSunburst.js';

  /** @type {{ segment: object|null }} */
  let { segment = null } = $props();

  /** The survey row behind the segment, or `null` for the "other" wedge. */
  let row = $derived(segment?.node ?? null);
</script>

<aside class="foundry detail" aria-live="polite">
  {#if !segment}
    <p class="empty">Hover or select a segment to see what is holding it.</p>
  {:else if segment.kind === 'other'}
    <h3>{segment.name}</h3>
    <p class="hint">
      Arcs too thin to hover. Every one is listed below, and all of them appear
      in the list view.
    </p>
    <ul class="members">
      {#each segment.members as member (member.id)}
        <li>
          <Badge tone={tierTone(member.tier)}>{tierLabel(member.tier)}</Badge>
          <code>{member.branch ?? member.path}</code>
          <span class="bytes">{formatBytes(member.bytes)}</span>
        </li>
      {/each}
    </ul>
  {:else if segment.kind === 'project'}
    <h3>{segment.name}</h3>
    <dl>
      <dt>path</dt><dd><code>{row?.path}</code></dd>
      <dt>size</dt><dd>{formatBytes(segment.bytes)}</dd>
      <dt>worktrees</dt><dd>{row?.worktrees?.length ?? 0}</dd>
    </dl>
  {:else}
    <h3>
      <Badge tone={tierTone(segment.tier)}>{tierLabel(segment.tier)}</Badge>
      {segment.name}
    </h3>
    <dl>
      <dt>path</dt><dd><code>{row?.path}</code></dd>
      <dt>branch</dt><dd>{row?.branch ?? '—'}</dd>
      <dt>size</dt>
      <dd>
        {formatBytes(segment.bytes)}
        {#if row?.size}
          <span class="note">
            {row.size.from_cache ? 'cached' : 'measured'}
            {row.size.truncated ? ', truncated' : ''}
            {row.size.unreadable ? `, ${row.size.unreadable} unreadable` : ''}
          </span>
        {/if}
      </dd>
      {#if row?.gate}
        <dt>gate</dt><dd><code>{row.gate}</code></dd>
      {/if}
      {#if row?.reason}
        <dt>reason</dt><dd>{row.reason}</dd>
      {/if}
      {#if row?.pr}
        <dt>PR</dt><dd>#{row.pr.number} {row.pr.state}</dd>
      {/if}
      {#if row?.session}
        <dt>session</dt><dd><code>{row.session}</code></dd>
      {/if}
    </dl>
    {#if row?.reasons?.length}
      <ul class="reasons">
        {#each row.reasons as reason, i (i)}
          <li><code>{reason.code}</code> {reason.detail}</li>
        {/each}
      </ul>
    {/if}
    <p class="hint">
      This view displays only. Clearing a worktree is a separate, confirmed
      action in the mpm dashboard.
    </p>
  {/if}
</aside>

<style>
  .detail {
    border: 1px solid var(--trusty-border);
    border-radius: 0.5rem;
    background: var(--trusty-card-bg);
    padding: 0.85rem 1rem;
    min-width: 0;
  }
  h3 {
    margin: 0 0 0.6rem;
    font-size: 0.92rem;
    font-weight: 600;
    color: var(--trusty-text-primary);
    display: flex;
    align-items: center;
    gap: 0.45rem;
    flex-wrap: wrap;
    overflow-wrap: anywhere;
  }
  .empty,
  .hint {
    margin: 0.5rem 0 0;
    font-size: 0.76rem;
    color: var(--trusty-text-secondary);
  }
  .empty {
    margin: 0;
  }
  dl {
    display: grid;
    grid-template-columns: minmax(0, auto) minmax(0, 1fr);
    gap: 0.25rem 0.7rem;
    margin: 0;
    font-size: 0.78rem;
  }
  dt {
    color: var(--trusty-text-muted);
    font-family: var(--trusty-mono, monospace);
    font-size: 0.7rem;
    text-transform: uppercase;
    letter-spacing: 0.06em;
  }
  dd {
    margin: 0;
    color: var(--trusty-text-primary);
    overflow-wrap: anywhere;
  }
  code {
    font-family: var(--trusty-mono, monospace);
    font-size: 0.73rem;
  }
  .note {
    color: var(--trusty-text-muted);
    font-size: 0.7rem;
  }
  .reasons,
  .members {
    list-style: none;
    margin: 0.6rem 0 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
    font-size: 0.75rem;
    color: var(--trusty-text-secondary);
  }
  .members li {
    display: flex;
    align-items: center;
    gap: 0.45rem;
    flex-wrap: wrap;
  }
  .bytes {
    margin-left: auto;
    font-family: var(--trusty-mono, monospace);
    color: var(--trusty-text-muted);
  }
</style>
