<svelte:head><title>История лимитов · canterlot</title></svelte:head>
<script lang="ts">
  import { resolve } from '$app/paths';
</script>
<main id="main" tabindex="-1"><h1>История лимитов</h1><p>График появится отдельным экраном.</p><a href={resolve('/sessions')}>К сессиям</a></main>
<style>
  main { padding: var(--s8); }
  a { color: var(--accent-link); }
</style>
