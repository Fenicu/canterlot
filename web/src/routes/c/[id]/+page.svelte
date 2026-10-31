<script lang="ts">
  import { page } from '$app/state';
  import { onMount } from 'svelte';
  import { clock } from '#lib/clock.js';
  import ChatGone from '#lib/screens/chat/ChatGone.svelte';
  import ChatScreen from '#lib/screens/chat/ChatScreen.svelte';
  import { leaseChatSession } from '#lib/screens/chat/open.js';
  import type { ChatStore } from '#lib/stores/chat.svelte.js';

  let chat = $state<ChatStore | null>(null);
  let error = $state('');
  let now = $state(clock.serverNow());
  const id = $derived(page.params.id ?? '');

  onMount(() => {
    const timer = setInterval(() => (now = clock.serverNow()), 1000);
    return () => clearInterval(timer);
  });

  $effect(() => {
    const current = id;
    chat = null;
    error = '';
    if (!current) return;
    const lease = leaseChatSession(current);
    let alive = true;
    lease.chat
      .then((store) => {
        if (alive) chat = store;
      })
      .catch(() => {
        if (alive) error = 'Не удалось открыть сессию';
      });
    return () => {
      alive = false;
      lease.release();
    };
  });
</script>

<svelte:head><title>Сессия</title></svelte:head>
<main id="main" class="chat-route">
  {#key id}
    {#if chat && chat.id === id && chat.gone}
      <ChatGone reason={chat.gone} />
    {:else if chat && chat.id === id}
      <ChatScreen {chat} {now} />
    {:else if error}
      <p>{error}</p>
    {:else}
      <p>Открываю сессию</p>
    {/if}
  {/key}
</main>
<style>
  .chat-route { height: 100%; min-height: 0; display: flex; flex-direction: column; }
</style>
