<script lang="ts">
  import MenuItem from '#lib/ui/MenuItem.svelte';
  import { setNotificationsMuted } from '#lib/screens/chat/controls.js';

  let {
    conversationId,
    muted,
    inline = false,
    ondone,
    onerror
  }: {
    conversationId: string;
    /** Пуши сессии и её делегатов уже выключены. */
    muted: boolean;
    /** Компактный вид для строки листа. */
    inline?: boolean;
    ondone?: () => void;
    onerror?: (error: unknown) => void;
  } = $props();

  let busy = $state(false);

  async function toggle(): Promise<void> {
    if (busy) return;
    busy = true;
    try {
      await setNotificationsMuted(conversationId, !muted);
      ondone?.();
    } catch (error) {
      onerror?.(error);
    } finally {
      busy = false;
    }
  }
</script>

{#if inline}
  <button type="button" class="inline" onclick={() => void toggle()}>
    {muted ? 'Включить уведомления' : 'Без уведомлений'}
  </button>
{:else}
  <MenuItem icon={muted ? 'bell' : 'bellOff'} onclick={() => void toggle()}>
    {muted ? 'Включить уведомления' : 'Без уведомлений'}
  </MenuItem>
{/if}

<style>
  .inline {
    min-height: var(--target);
    padding-inline: var(--s2);
    border: var(--s0);
    border-radius: var(--radius-sm);
    background: var(--transparent);
    color: var(--accent-link);
    font-size: var(--fs-meta);
    white-space: nowrap;
  }
</style>
