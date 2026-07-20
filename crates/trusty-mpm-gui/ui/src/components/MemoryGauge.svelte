<script lang="ts">
  // Why: Memory pressure must be readable at a glance in the dense session
  // list; a thin colored bar conveys it without taking vertical space.
  // What: Renders a horizontal bar filled to `pct`%, green normally, amber at
  // >70%, red at >85%.
  // Test: Pass pct=50 → green fill at 50% width; pct=75 → amber; pct=90 → red.
  export let pct: number = 0;

  $: clamped = Math.max(0, Math.min(100, pct));
  $: tone =
    clamped > 85
      ? 'bg-status-error'
      : clamped > 70
        ? 'bg-status-paused'
        : 'bg-status-running';
</script>

<div
  class="h-1 w-full overflow-hidden rounded-full bg-trusty-border"
  title={`memory ${clamped}%`}
>
  <div
    class={`h-full rounded-full transition-all duration-300 ${tone}`}
    style={`width: ${clamped}%`}
  ></div>
</div>
