<svelte:head><title>Машины · canterlot</title></svelte:head>
<script lang="ts">
  import { resolve } from '$app/paths';
</script>
<main id="main" tabindex="-1"><h1>Машины</h1><p>Список машин появится отдельным экраном.</p><a href={resolve('/sessions')}>К сессиям</a></main>
<style>
  main { padding: var(--s8); }
  a { color: var(--accent-link); }
</style>
