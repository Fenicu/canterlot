<script lang="ts">
  import { page } from '$app/state';
  import { onMount } from 'svelte';
  import ChatScreen from '$lib/screens/chat/ChatScreen.svelte';
  import { openChatSession } from '$lib/screens/chat/open';
  import type { ChatStore } from '$lib/stores/chat.svelte';

  let chat = $state<ChatStore | null>(null);
  let error = $state('');
  const id = $derived(page.params.id ?? '');

  onMount(() => {
    let alive = true;
    openChatSession(id)
      .then((store) => {
        if (alive) chat = store;
      })
      .catch(() => {
        error = 'Не удалось открыть сессию';
      });
    return () => {
      alive = false;
    };
  });
</script>

<svelte:head><title>Сессия</title></svelte:head>
<main id="main">
  {#if chat}
    <ChatScreen {chat} />
  {:else if error}
    <p>{error}</p>
  {:else}
    <p>Открываю сессию</p>
  {/if}
</main>
