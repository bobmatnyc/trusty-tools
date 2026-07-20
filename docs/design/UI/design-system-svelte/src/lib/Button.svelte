<script>
  // variant: '' | 'primary' | 'danger' | 'ghost'; size: '' | 'sm'
  let { variant = '', size = '', disabled = false, onclick, children } = $props();
</script>

<button class="btn {variant ? 'btn-' + variant : ''} {size ? 'btn-' + size : ''}" {disabled} {onclick}>
  {@render children?.()}
</button>
