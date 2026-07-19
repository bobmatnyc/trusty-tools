<script lang="ts">
  /**
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
  <RobotIcon {size} variant="mono" color="#B7410E" />
  <span class="text-sm">
    <span class="font-normal">Trusty </span><span class="font-semibold">Assistant</span>
  </span>
</span>
