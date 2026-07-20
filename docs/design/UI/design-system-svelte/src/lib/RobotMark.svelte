<script>
  // Foundry robot mark. eyes: 'square' | 'round' | 'visor'.
  // state: 'idle' | 'receiving' | 'working' | 'none' (maps to foundry.css robot-* animations)
  let {
    size = 40,
    body = 'var(--trusty-accent)',
    face = '#f5efe7',
    eyes = 'square',
    antenna = true,
    antennaColor = '#e9b98a',
    state = 'none'
  } = $props();
  const u = $derived(size / 40);
</script>

<div
  class="robot"
  class:robot-idle={state === 'idle'}
  class:robot-working={state === 'working'}
  class:robot-receiving={state === 'receiving'}
  style="width:{size}px;height:{size}px;background:{body};border-radius:{6 * u}px;"
>
  {#if antenna}
    <span class="stem" style="top:{-6 * u}px;width:{Math.max(2, 2 * u)}px;height:{7 * u}px;background:{antennaColor};"></span>
    <span class="tip robot-antenna-tip" style="top:{-10 * u}px;width:{5 * u}px;height:{5 * u}px;background:{antennaColor};"></span>
    {#if state === 'receiving'}<span class="robot-ring"></span><span class="robot-ring"></span>{/if}
  {/if}
  <span class="robot-eye" style="left:{9 * u}px;top:{eyes === 'visor' ? 15 * u : 13 * u}px;width:{7 * u}px;height:{(eyes === 'visor' ? 3 : 7) * u}px;background:{face};border-radius:{eyes === 'round' ? '50%' : '0'};"></span>
  <span class="robot-eye" style="right:{9 * u}px;top:{eyes === 'visor' ? 15 * u : 13 * u}px;width:{7 * u}px;height:{(eyes === 'visor' ? 3 : 7) * u}px;background:{face};border-radius:{eyes === 'round' ? '50%' : '0'};"></span>
  <span class="mouth" style="left:{12 * u}px;right:{12 * u}px;bottom:{9 * u}px;height:{3 * u}px;background:{face};"></span>
</div>

<style>
  .robot { position: relative; flex: none; }
  .robot > span { position: absolute; display: block; }
  .stem, .tip { left: 50%; transform: translateX(-50%); }
  .tip { border-radius: 50%; }
</style>
