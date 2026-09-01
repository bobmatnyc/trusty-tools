<script>
  /**
   * Why: The header is the one place that answers "what app is this", so it
   *      carries the product lockup rather than a bare `<h1>`. The full
   *      horizontal lockup asset (`docs/design/UI/icons/trusty-console-logo.svg`)
   *      sets its wordmark as SVG `<text>`, which needs the Chakra Petch and
   *      IBM Plex Mono webfonts; this SPA is served from an embedded bundle and
   *      ships no font files, so the lockup is reassembled here as mark + HTML
   *      text on the documented fallback stacks. `trusty-agents` solves the
   *      same constraint the same way in its `LogoMark.svelte`.
   * What: Renders `BrandMark` beside the "Trusty Console" wordmark and the
   *       "UNIT-05 · SERVICE CONSOLE" descriptor, spaced to the identity
   *       README's clear-space rule (one robot-eye width around the lockup).
   *       Colors come from the palette tokens, so both fields are covered by
   *       one component. The descriptor also names the running crate version,
   *       probed from the server on mount (`consoleVersion.js`) and rendered in
   *       the same span, so it inherits the descriptor's type style; until that
   *       probe answers, the descriptor reads exactly as it did before.
   * Test: Mount `<BrandLockup />`, flip `<html data-theme>` between light and
   *       dark, and confirm the wordmark stays primary-text and the descriptor
   *       stays muted in both, with the mark recoloring alongside. The
   *       descriptor text itself is covered by `consoleVersion.test.js`.
   */
  import { onMount } from 'svelte';

  import BrandMark from './BrandMark.svelte';
  import { describeConsole, fetchConsoleVersion } from './consoleVersion.js';

  /** The running server's version, or `null` while unknown. */
  let version = $state(null);

  onMount(async () => {
    version = await fetchConsoleVersion();
  });
</script>

<span class="lockup">
  <BrandMark size={44} />
  <span class="text">
    <span class="wordmark">Trusty Console</span>
    <span class="descriptor">{describeConsole(version)}</span>
  </span>
</span>

<style>
  .lockup {
    display: inline-flex;
    align-items: center;
    gap: 0.7rem;
    /* Clear space = one robot-eye width at this scale (identity README). */
    padding: 0.3rem 0;
  }
  .text {
    display: flex;
    flex-direction: column;
    gap: 0.15rem;
    min-width: 0;
  }
  .wordmark {
    font-family: 'Chakra Petch', 'Arial Narrow', sans-serif;
    font-size: 1.6rem;
    font-weight: 700;
    letter-spacing: 0.05em;
    line-height: 1.05;
    text-transform: uppercase;
    color: var(--trusty-text-primary);
  }
  .descriptor {
    font-family: 'IBM Plex Mono', ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 0.66rem;
    font-weight: 600;
    letter-spacing: 0.16em;
    color: var(--trusty-text-secondary);
  }
</style>
