<script lang="ts">
  /**
   * Why: The logo mark (robot + "open-mpm" wordmark) is the canonical product
   * lockup used in the sidebar header. Bundling icon + wordmark together
   * guarantees consistent spacing and weight across the app.
   * What: Renders the RobotIcon (28px by default) inline with the wordmark
   * "open-" (regular) + "mpm" (semibold), both in the ompm-text color.
   * Test: Mount <LogoMark /> and visually confirm "mpm" appears bolder than
   * "open-" and the robot icon is left-aligned with the text baseline.
   */
  import RobotIcon from './RobotIcon.svelte';

  export let size: number = 28;
</script>

<span class="inline-flex items-center gap-2 text-ompm-text">
  <RobotIcon {size} variant="mono" color="#3B4CCA" />
  <span class="text-sm">
    <span class="font-normal">open-</span><span class="font-semibold">mpm</span>
  </span>
</span>
