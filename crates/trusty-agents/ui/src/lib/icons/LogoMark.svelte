<script lang="ts">
  /**
   * Canonical source: docs/design/UI/design-system/icons/LogoMark.svelte
   * (refs #3486). This is a vendored copy — the canonical version is styled
   * with plain CSS custom properties; this copy intentionally re-expresses
   * the same lockup against this crate's own Tailwind token layer
   * (`text-foundry-text`, `font-display`) instead. Keep the visual result
   * (spacing, weights, wordmark text) in sync with the canonical file until
   * #3492 (`@trusty/foundry` package) replaces copy-paste vendoring with a
   * real import.
   *
   * Why: The logo mark (robot + "Trusty Assistant" wordmark) is the canonical
   * product lockup used in the sidebar header. Bundling icon + wordmark
   * together guarantees consistent spacing and weight across the app.
   * What: Renders the RobotIcon (28px by default) inline with the wordmark
   * "Trusty" (regular) + "Assistant" (semibold), both in the foundry-text
   * color, using the Chakra Petch display face.
   * Test: Mount <LogoMark /> and visually confirm "Assistant" appears bolder
   * than "Trusty" and the robot icon is left-aligned with the text baseline.
   */
  import RobotIcon from './RobotIcon.svelte';

  export let size: number = 28;
</script>

<span class="inline-flex items-center gap-2 text-foundry-text font-display">
  <RobotIcon {size} variant="mono" color="var(--color-primary)" />
  <span class="text-sm">
    <span class="font-normal">Trusty </span><span class="font-semibold">Assistant</span>
  </span>
</span>
