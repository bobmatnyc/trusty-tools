<script lang="ts">
  /**
   * Why (brand identity wiring): the app previously hand-built its brand
   * lockup out of a generic mono robot glyph component + literal
   * "TRUSTY AGENTS" text (Header.svelte), independent of the actual
   * Trusty Agents identity suite
   * delivered in `docs/design/UI/icons/`. That suite is the source of truth
   * for the mark, wordmark, and "UNIT-04 · MPM ORCHESTRATION" descriptor —
   * this component wires the real lockup in so the brand is consistent and
   * centrally maintained (one place to update if the identity changes).
   * What: Inlines the two full horizontal lockups verbatim from
   * `docs/design/UI/icons/trusty-agents-logo.svg` (paper/light fields) and
   * `trusty-agents-logo-reversed.svg` (Night Shift dark fields), toggling
   * visibility with the app's existing `dark:` Tailwind convention
   * (`stores/theme.ts` toggles the `.dark` class on `<html>`, matched by
   * `tailwind.config.js`'s `darkMode: 'class'` — the same mechanism every
   * other themed element in this app already uses). Both lockups share the
   * source SVG's `viewBox="0 0 600 112"`; only `height` is exposed as a prop
   * so the aspect ratio (and the identity suite's fixed clear-space) is
   * never distorted. Per `docs/design/UI/icons/README.md`: do not recolor
   * the mark, add shadows, or drop the `UNIT-04` descriptor.
   *
   * Legibility fix (owner visual feedback on #3479): the source asset's
   * "UNIT-04 · MPM ORCHESTRATION" descriptor (`font-size="11"`,
   * `font-weight="600"`) rendered at ~4-5 physical px at header scale —
   * illegible in both themes despite acceptable color contrast (computed
   * ~6.6:1 light / ~5.1:1 dark against the header surface). The mark and
   * "TRUSTY AGENTS" wordmark are reproduced byte-for-byte from the source
   * SVGs; only the descriptor's `font-size` (11→20), `font-weight`
   * (600→700), and fill color (darkened light / lightened dark, for extra
   * contrast margin at the larger size) are intentionally adjusted here for
   * legibility — text is explicitly exempted from the "no recolor" rule,
   * which applies to the robot mark.
   * Test: Mount `<Logo />`, toggle `ThemeToggle` through light/dark, and
   * confirm the rust-on-paper lockup shows in light mode and the
   * light-text-on-oxide reversed lockup shows in dark mode, with the
   * "UNIT-04 · MPM ORCHESTRATION" descriptor clearly readable in both.
   */
  export let height: number = 28;
</script>

<svg
  class="block dark:hidden"
  style:height="{height}px"
  style:width="auto"
  xmlns="http://www.w3.org/2000/svg"
  viewBox="0 0 600 112"
  role="img"
  aria-labelledby="trusty-agents-logo-title trusty-agents-logo-desc"
>
  <title id="trusty-agents-logo-title">Trusty Agents</title>
  <desc id="trusty-agents-logo-desc">Trusty Agents logo with Foundry UNIT-04 robot mark.</desc>
  <g transform="translate(8 8)">
    <path d="M40 24V12" fill="none" stroke="#B7410E" stroke-width="4" />
    <rect x="36" y="5" width="8" height="8" rx="2" fill="#B7410E" />
    <rect x="8" y="24" width="64" height="56" rx="9" fill="#B7410E" />
    <rect x="20" y="40" width="9" height="9" rx="1.5" fill="#FFFDF9" />
    <rect x="51" y="40" width="9" height="9" rx="1.5" fill="#FFFDF9" />
    <path d="M24 66h32" fill="none" stroke="#FFFDF9" stroke-width="4" stroke-linecap="square" />
  </g>
  <text x="104" y="51" fill="#2B1C12" font-family="'Chakra Petch', 'Arial Narrow', sans-serif" font-size="30" font-weight="700" letter-spacing="1.4">TRUSTY AGENTS</text>
  <text x="106" y="80" fill="#5C4630" font-family="'IBM Plex Mono', monospace" font-size="20" font-weight="700" letter-spacing="1.6">UNIT-04 · MPM ORCHESTRATION</text>
</svg>

<svg
  class="hidden dark:block"
  style:height="{height}px"
  style:width="auto"
  xmlns="http://www.w3.org/2000/svg"
  viewBox="0 0 600 112"
  role="img"
  aria-labelledby="trusty-agents-logo-reversed-title trusty-agents-logo-reversed-desc"
>
  <title id="trusty-agents-logo-reversed-title">Trusty Agents</title>
  <desc id="trusty-agents-logo-reversed-desc">Trusty Agents logo for use on dark Night Shift surfaces.</desc>
  <g transform="translate(8 8)">
    <path d="M40 24V12" fill="none" stroke="#D97742" stroke-width="4" />
    <rect x="36" y="5" width="8" height="8" rx="2" fill="#D97742" />
    <rect x="8" y="24" width="64" height="56" rx="9" fill="#D97742" />
    <rect x="20" y="40" width="9" height="9" rx="1.5" fill="#201612" />
    <rect x="51" y="40" width="9" height="9" rx="1.5" fill="#201612" />
    <path d="M24 66h32" fill="none" stroke="#201612" stroke-width="4" stroke-linecap="square" />
  </g>
  <text x="104" y="51" fill="#F0E7D8" font-family="'Chakra Petch', 'Arial Narrow', sans-serif" font-size="30" font-weight="700" letter-spacing="1.4">TRUSTY AGENTS</text>
  <text x="106" y="80" fill="#C7A886" font-family="'IBM Plex Mono', monospace" font-size="20" font-weight="700" letter-spacing="1.6">UNIT-04 · MPM ORCHESTRATION</text>
</svg>
