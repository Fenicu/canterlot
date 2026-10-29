<script lang="ts">
  import type { ChatStore } from '$lib/stores/chat.svelte';
  import { deliveryLabel, scheduledLabel } from '$lib/timeline/present';

  let { chat }: { chat: ChatStore } = $props();
  const items = $derived(chat.strip());
  const scheduled = $derived(chat.scheduled as { id?: string; send_at?: string; text?: string }[]);
</script>

<div class="strip">
  {#each items as input (input.key)}
    <p data-state={input.state}>{deliveryLabel(input.state)} {input.text}</p>
  {/each}
  {#each scheduled as item, index (item.id ?? index)}
    <p>{item.send_at ? scheduledLabel(Date.parse(item.send_at)) : 'Отправится позже'} {item.text ?? ''}</p>
  {/each}
</div>

<style>
  .strip { display: flex; flex-direction: column; gap: var(--s2); }
  p { margin: 0; }
</style>
