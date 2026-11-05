<script lang="ts">
  import { page } from '$app/state';
  import { onMount } from 'svelte';
  import { clock } from '#lib/clock.js';
  import { ticker } from '#lib/ticker.svelte.js';
  import ChatGone from '#lib/screens/chat/ChatGone.svelte';
  import ChatTrashed from '#lib/screens/chat/ChatTrashed.svelte';
  import ChatScreen from '#lib/screens/chat/ChatScreen.svelte';
  import { leaseChatSession } from '#lib/screens/chat/open.js';
  import type { ChatStore } from '#lib/stores/chat.svelte.js';

  let chat = $state<ChatStore | null>(null);
  let error = $state('');
  const now = $derived.by(() => {
    void ticker.second;
    return clock.serverNow();
  });
  const id = $derived(page.params.id ?? '');

  onMount(() => ticker.retain());

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
    {:else if chat && chat.id === id && chat.trashed}
      <ChatTrashed id={chat.id} />
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
