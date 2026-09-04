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
   *
   *       #6733: the robot is alive. The three console readouts light in
   *       sequence on a 1.8s loop, and the whole unit rocks once every 16s.
   *       Both animations live in this component's scoped CSS, so every render
   *       site inherits them from one definition — the header lockup
   *       (`BrandLockup.svelte`, itself used by `App.svelte` and
   *       `Screensaver.svelte`) and the overview loading panel
   *       (`App.svelte`). Nothing but `transform` and `opacity` animates, so
   *       the mark's box never changes size and nothing around it moves.
   * Test: Mount `<BrandMark size={64} />` under `<html data-theme="light">` and
   *       again under `data-theme="dark"`; the chassis must read #b7410e then
   *       #d97742, with the readouts inverting to match. For #6733, screenshot
   *       `/ui` at two points in the readout cycle — a different one of the
   *       three panel bars is bright in each — and once with
   *       `prefers-reduced-motion: reduce` emulated, where all three sit at
   *       full opacity and the mark sits square.
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
  <!-- #6733: the three console readouts are the cycling element; the stagger
       is the only thing that differs between them. -->
  <rect class="face readout r1" x="10" y="51" width="12" height="5" rx="1" />
  <rect class="face readout r2" x="26" y="51" width="12" height="5" rx="1" />
  <rect class="face readout r3" x="42" y="51" width="12" height="5" rx="1" />
</svg>

<style>
  /* Presentation attributes are avoided for color so the mark tracks the
     palette tokens; scoped CSS wins over any inherited fill. */
  .chassis { fill: var(--trusty-accent); }
  .chassis-stroke { stroke: var(--trusty-accent); }
  .face { fill: var(--trusty-mark-face); }
  .face-stroke { stroke: var(--trusty-mark-face); }
  svg { display: block; flex-shrink: 0; }

  /* #6733: the robot animates. Both animations are declared once, here, and
     reach every render site through this component — there is no per-site
     copy to drift. Only `transform` and `opacity` animate, which the
     compositor handles without laying the page out again, so the mark's box
     is the same size at every frame and nothing beside it shifts. No color is
     introduced: the readouts keep `--trusty-mark-face` and vary only in
     opacity, so the mark still recolors with the palette. */

  /* The whole unit rocks and settles. `transform-origin` sits at the bottom
     centre of the console panel so the robot pivots on its own base rather
     than spinning about the icon's middle. The motion occupies the last 18%
     of a 16s cycle — about 2.9s of movement, then ~13s square — so it reads
     as an occasional glance rather than a wobble. */
  svg {
    transform-origin: 50% 92%;
    will-change: transform;
    animation: mark-tilt 16s ease-in-out infinite;
  }

  @keyframes mark-tilt {
    0%, 82% { transform: rotate(0deg); }
    86% { transform: rotate(-5deg); }
    90% { transform: rotate(4deg); }
    94% { transform: rotate(-2deg); }
    100% { transform: rotate(0deg); }
  }

  /* One readout is lit and raised at a time. Three bars share one 1.8s
     keyframe and are offset by a third of it each with a negative delay, so
     the sequence is already mid-cycle on the first frame — no dark start. */
  .readout {
    opacity: 0.3;
    will-change: transform, opacity;
    animation: mark-readout 1.8s ease-in-out infinite;
  }
  .r1 { animation-delay: 0s; }
  .r2 { animation-delay: -1.2s; }
  .r3 { animation-delay: -0.6s; }

  @keyframes mark-readout {
    0% { opacity: 0.3; transform: translateY(0); }
    10%, 26% { opacity: 1; transform: translateY(-2px); }
    38%, 100% { opacity: 0.3; transform: translateY(0); }
  }

  /* #6733 AC3: motion is decorative here, so it stops entirely rather than
     slowing down. With the animations off the readouts return to the full
     opacity the static mark shipped with and the unit sits square, which is
     exactly the pre-#6733 rendering. */
  @media (prefers-reduced-motion: reduce) {
    svg { animation: none; transform: none; will-change: auto; }
    .readout {
      animation: none;
      opacity: 1;
      transform: none;
      will-change: auto;
    }
  }
</style>
