<script>
  /**
   * Why: Constrained spots — the header lockup, the overview loading panel —
   *      need the standalone Trusty Console operator mark rather than the full
   *      horizontal lockup, per `docs/design/UI/icons/README.md` ("standalone
   *      operator mark for constrained spaces"). The console persona is one
   *      robot seated at a dashboard panel; it shares the agents suite's head
   *      geometry so the two read as the same machine doing different jobs.
   * What: Inlines `docs/design/UI/icons/trusty-console-mark.svg` at the
   *       requested `size`, with the source SVG's two fixed brand colors
   *       swapped for `--trusty-accent` (rust chassis) and `--trusty-mark-face`
   *       (paper/oxide eyes, mouth, readouts). That is what makes one asset
   *       serve both fields: the tokens already carry the Foundry rust for each
   *       palette, so the mark recolors with the theme rather than shipping a
   *       reversed twin. Per the identity README: do not recolor the robot to
   *       anything else, add shadows, or place the mark in a circle.
   * Test: Mount `<BrandMark size={64} />` under `<html data-theme="light">` and
   *       again under `data-theme="dark"`; the chassis must read #b7410e then
   *       #d97742, with the readouts inverting to match.
   *
   * @type {{ size?: number }}
   */
  let { size = 20 } = $props();
</script>

<svg
  width={size}
  height={size}
  xmlns="http://www.w3.org/2000/svg"
  viewBox="0 0 64 64"
  role="img"
  aria-labelledby="trusty-console-mark-title trusty-console-mark-desc"
>
  <title id="trusty-console-mark-title">Trusty Console robot mark</title>
  <desc id="trusty-console-mark-desc">
    Foundry UNIT robot seated at a console panel with three readouts.
  </desc>
  <path class="chassis-stroke" d="M32 13V6" fill="none" stroke-width="3" />
  <rect class="chassis" x="29" y="1" width="6" height="6" rx="1.5" />
  <rect class="chassis" x="12" y="13" width="40" height="30" rx="6" />
  <rect class="face" x="20" y="23" width="7" height="7" rx="1" />
  <rect class="face" x="37" y="23" width="7" height="7" rx="1" />
  <path class="face-stroke" d="M25 35h14" fill="none" stroke-width="3" stroke-linecap="square" />
  <rect class="chassis" x="15" y="41" width="5" height="8" rx="2" />
  <rect class="chassis" x="44" y="41" width="5" height="8" rx="2" />
  <rect class="chassis" x="4" y="47" width="56" height="13" rx="4" />
  <rect class="face" x="10" y="51" width="12" height="5" rx="1" />
  <rect class="face" x="26" y="51" width="12" height="5" rx="1" />
  <rect class="face" x="42" y="51" width="12" height="5" rx="1" />
</svg>

<style>
  /* Presentation attributes are avoided for color so the mark tracks the
     palette tokens; scoped CSS wins over any inherited fill. */
  .chassis { fill: var(--trusty-accent); }
  .chassis-stroke { stroke: var(--trusty-accent); }
  .face { fill: var(--trusty-mark-face); }
  .face-stroke { stroke: var(--trusty-mark-face); }
  svg { display: block; flex-shrink: 0; }
</style>
