<script lang="ts">
  import { fmt } from '#lib/format/ru.js';
  import type { ChatStore } from '#lib/stores/chat.svelte.js';
  import IconButton from '#lib/ui/IconButton.svelte';
  import AlarmsList from './AlarmsList.svelte';

  let { chat, onclose }: { chat: ChatStore; onclose?: () => void } = $props();

  const count = $derived(
    chat.alarms.filter((alarm) => alarm.state === 'armed' || alarm.state === 'failed').length
  );
  const countLabel = $derived(
    `${count} ${fmt.plural(count, ['будильник', 'будильника', 'будильников'])}`
  );
</script>

<aside class="desktop-lane-panel" aria-label="Будильники" data-alarms-panel>
  <header class="topbar">
    <div class="titles">
      <h2 class="panel-title">Будильники</h2>
      <p class="count">{countLabel}</p>
    </div>
    {#if onclose}
      <IconButton icon="close" label="Закрыть боковую панель" onclick={onclose} />
    {/if}
  </header>
  <div class="body">
    <AlarmsList {chat} />
  </div>
</aside>

<style>
  .desktop-lane-panel {
    width: 100%;
    height: 100%;
    display: flex;
    flex-direction: column;
    min-width: 0;
    min-height: 0;
    color: var(--text);
  }
  .topbar {
    display: flex;
    align-items: center;
    gap: var(--s4);
    min-height: var(--target);
    margin: var(--s5) var(--gutter) var(--s3);
    padding-block: var(--s2);
  }
  .topbar > :global(:last-child) {
    margin-right: var(--icon-hang);
  }
  .titles {
    flex: 1;
    min-width: 0;
  }
  .panel-title {
    margin: var(--s0);
    font-size: var(--fs-title);
    font-weight: var(--weight-semibold);
  }
  .count {
    margin: var(--s1) var(--s0) var(--s0);
    color: var(--muted);
    font-size: var(--fs-sm);
  }
  .body {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: var(--s0) var(--gutter) var(--s4);
  }
</style>
