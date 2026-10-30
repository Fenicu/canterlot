<script lang="ts">
  import NewSession from '#lib/screens/sessions/NewSession.svelte';
</script>
<svelte:head><title>Новая сессия · canterlot</title></svelte:head>
<main id="main" tabindex="-1" data-new-session><NewSession /></main>
<style>
  main { height: 100%; min-height: 0; overflow: auto; }
</style>
