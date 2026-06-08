<script>
  import { onMount } from 'svelte';
  import ServiceCard from './ServiceCard.svelte';

  let services = $state([]);
  let loading = $state(true);
  let error = $state(null);

  onMount(async () => {
    try {
      const resp = await fetch('/api/console/services');
      if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
      services = await resp.json();
    } catch (e) {
      error = e.message;
    } finally {
      loading = false;
    }
  });
</script>

<main>
  <header>
    <h1>Trusty Console</h1>
    <p class="subtitle">Service status overview</p>
  </header>

  {#if loading}
    <div class="loading">Detecting services…</div>
  {:else if error}
    <div class="error">Failed to load services: {error}</div>
  {:else}
    <div class="cards">
      {#each services as service (service.id)}
        <ServiceCard {service} />
      {/each}
    </div>
  {/if}
</main>

<style>
  :global(*, *::before, *::after) {
    box-sizing: border-box;
  }
  :global(body) {
    margin: 0;
    font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
    background: #0f1117;
    color: #e2e8f0;
    min-height: 100vh;
  }
  main {
    max-width: 960px;
    margin: 0 auto;
    padding: 2rem 1rem;
  }
  header {
    margin-bottom: 2rem;
  }
  h1 {
    font-size: 2rem;
    font-weight: 700;
    margin: 0 0 0.25rem;
    background: linear-gradient(135deg, #7c3aed, #2563eb);
    -webkit-background-clip: text;
    -webkit-text-fill-color: transparent;
    background-clip: text;
  }
  .subtitle {
    color: #94a3b8;
    margin: 0;
  }
  .cards {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(280px, 1fr));
    gap: 1rem;
  }
  .loading,
  .error {
    padding: 1.5rem;
    border-radius: 0.5rem;
    background: #1e2130;
    color: #94a3b8;
  }
  .error {
    color: #f87171;
  }
</style>
