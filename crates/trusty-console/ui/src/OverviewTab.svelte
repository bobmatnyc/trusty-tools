<script>
  import ServiceCard from './ServiceCard.svelte';

  /**
   * @type {{ services: Array<{id: string, display_name: string, status: string, version?: string, url?: string}>, onSelectService: (id: string) => void }}
   */
  let { services, onSelectService } = $props();
</script>

<div class="overview">
  {#if services.length === 0}
    <div class="empty">No services detected.</div>
  {:else}
    <div class="cards">
      {#each services as service (service.id)}
        <ServiceCard {service} onViewDetails={onSelectService} />
      {/each}
    </div>
  {/if}
</div>

<style>
  .cards {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(280px, 1fr));
    gap: 1rem;
  }
  .empty {
    padding: 1.5rem;
    border-radius: 0.5rem;
    background: #1e2130;
    color: #94a3b8;
  }
</style>
