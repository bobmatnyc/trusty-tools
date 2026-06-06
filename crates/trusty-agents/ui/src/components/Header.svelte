<script lang="ts">
  /**
   * Why: A persistent top bar gives the app a consistent identity (logo on
   * the left) and a single, predictable place for global controls (theme
   * toggle on the right). Previously the theme toggle was buried in the
   * sidebar footer where users couldn't find it; lifting it to the header
   * makes the affordance discoverable from anywhere in the app.
   * What: Sticky `h-12` bar spanning the full width above the sidebar+main
   * split. Renders <LogoMark/> on the left and <ThemeToggle/> on the right.
   * Test: Mount <Header/>, verify the LogoMark appears at the left edge and
   * the ThemeToggle pill at the right edge; clicking Light/Dark/System
   * still flips the page palette via the existing theme store.
   */
  import LogoMark from '../lib/icons/LogoMark.svelte';
  import ThemeToggle from './ThemeToggle.svelte';
</script>

<header
  class="sticky top-0 z-20 flex h-12 w-full items-center justify-between border-b border-ompm-light-border dark:border-ompm-border bg-ompm-light-surface dark:bg-ompm-surface px-4"
>
  <div class="flex items-center">
    <LogoMark size={22} />
  </div>
  <div class="flex items-center">
    <ThemeToggle />
  </div>
</header>
