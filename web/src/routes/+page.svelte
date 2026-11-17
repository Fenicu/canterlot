<script lang="ts">
  import { goto } from '$app/navigation';
  import { resolve } from '$app/paths';
  import { onMount } from 'svelte';
  onMount(() => {
    void goto(resolve('/sessions'), { replaceState: true });
  });
</script>
<svelte:head><title>Canterlot</title></svelte:head>
<main id="main" tabindex="-1"><p>Выберите сессию</p></main>
