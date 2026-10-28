<script lang="ts">
  import TopBar from '$lib/ui/TopBar.svelte';
  import IconButton from '$lib/ui/IconButton.svelte';
  import { theme } from '$lib/theme';
</script>
<svelte:head><title>Сессии · canterlot</title></svelte:head>
<TopBar title="Сессии">{#snippet actions()}<IconButton icon={theme.effective === 'dark' ? 'sun' : 'moon'} label="Сменить тему" onclick={() => theme.toggle()} />{/snippet}</TopBar>
<main id="main" tabindex="-1"><h2>Список сессий появится здесь</h2><p>Подключение к хабу будет добавлено на следующем этапе.</p></main>
<style>
  main { max-width: var(--reading-max); margin-inline: auto; padding: var(--s9) var(--s8); }
  p { margin-top: var(--s4); color: var(--secondary); }
</style>
