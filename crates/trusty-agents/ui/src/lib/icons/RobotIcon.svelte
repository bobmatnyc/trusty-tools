<script lang="ts">
  /**
   * Why: The robot mark is the open-mpm visual identity — a friendly PM
   * orchestrator face that appears in the sidebar header, splash, and badges.
   * Centralizing it as a component lets us swap the entire brand mark in one
   * place and reuse it at multiple sizes/variants without duplicating SVG.
   * What: Renders a 32x32 robot face SVG (rounded-square head, two eyes,
   * `>_` terminal mouth, antenna w/ dot tip) in one of three variants:
   * - "full":  filled dark bg square + colored stroke/details (hero usage)
   * - "mono":  transparent fill, single-color stroke (inline w/ text)
   * - "badge": filled colored circle, white details (chip / avatar)
   * Test: Render <RobotIcon /> with each variant and visually confirm the
   * antenna dot, eyes, and `>_` mouth render at 32px without clipping.
   */
  export let size: number = 32;
  export let color: string = '#3B4CCA';
  export let variant: 'full' | 'mono' | 'badge' = 'full';

  // Derived presentation tokens per variant.
  $: bgFill = variant === 'full' ? '#1A1F3A' : variant === 'badge' ? color : 'none';
  $: stroke = variant === 'badge' ? '#FFFFFF' : color;
  $: dotFill = variant === 'badge' ? '#FFFFFF' : color;
</script>

<svg
  xmlns="http://www.w3.org/2000/svg"
  width={size}
  height={size}
  viewBox="0 0 32 32"
  fill="none"
  stroke={stroke}
  stroke-width="1.5"
  stroke-linecap="round"
  stroke-linejoin="round"
  aria-label="open-mpm robot"
  role="img"
>
  {#if variant === 'badge'}
    <!-- Badge: colored circle container -->
    <circle cx="16" cy="16" r="14" fill={bgFill} stroke="none" />
  {:else}
    <!-- Full / mono: rounded square face -->
    <rect x="6" y="9" width="20" height="18" rx="4" fill={bgFill} stroke={stroke} />
  {/if}

  <!-- Antenna -->
  <line x1="16" y1="9" x2="16" y2="4" stroke={stroke} />
  <circle cx="16" cy="3" r="1.25" fill={dotFill} stroke="none" />

  <!-- Eyes -->
  <circle cx="12" cy="16" r="1.25" fill={dotFill} stroke="none" />
  <circle cx="20" cy="16" r="1.25" fill={dotFill} stroke="none" />

  <!-- Mouth: chevron `>` + underscore `_` -->
  <polyline points="11.5,21 13.5,22.5 11.5,24" stroke={stroke} fill="none" />
  <line x1="15" y1="24" x2="20.5" y2="24" stroke={stroke} />

  {#if variant === 'full'}
    <!-- Sparkle accent (only on full hero variant) -->
    <path d="M26 6 L26 9 M24.5 7.5 L27.5 7.5" stroke={color} stroke-width="1.25" />
  {/if}
</svg>
