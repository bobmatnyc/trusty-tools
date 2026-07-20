<script lang="ts">
  /**
   * Why: The logo mark (brand mark + "Trusty Agents" wordmark) is the
   * canonical product lockup used in the sidebar header — a constrained
   * space, per `docs/design/UI/icons/README.md`, where the full horizontal
   * lockup (`Logo.svelte`, used in `Header.svelte`) doesn't fit. Bundling
   * mark + wordmark together guarantees consistent spacing and weight across
   * the app. Previously this rendered the generic, pre-identity-suite
   * mono robot glyph; it now uses the actual Trusty Agents brand mark asset
   * (`docs/design/UI/icons/trusty-agents-mark.svg`). This crate intentionally
   * diverges from the generic Foundry `LogoMark` glyph vendored
   * at `docs/design/UI/design-system/icons/` (#3486/#3495) — trusty-agents
   * carries its own product identity rather than the placeholder glyph, so
   * it is exempt from that file's "must stay in sync" vendoring convention.
   * What: Renders `Mark` (28px by default) inline with the wordmark
   * "Trusty" (regular) + "Agents" (semibold), both in the foundry-text
   * color, using the Chakra Petch display face — matching the "TRUSTY
   * AGENTS" wordmark in the full lockup (`Logo.svelte`).
   * Test: Mount <LogoMark /> and visually confirm "Agents" appears bolder
   * than "Trusty" and the brand mark is left-aligned with the text baseline.
   */
  import Mark from './Mark.svelte';

  export let size: number = 28;
</script>

<span class="inline-flex items-center gap-2 text-foundry-text font-display">
  <Mark {size} />
  <span class="text-sm">
    <span class="font-normal">Trusty </span><span class="font-semibold">Agents</span>
  </span>
</span>
