<script lang="ts">
  import type { ChatStore } from '#lib/stores/chat.svelte.js';
  import Sheet from '#lib/ui/Sheet.svelte';
  import { stopGoal, controlBusy } from './controls';
  import { goalState, goalUsage } from './goalView';

  let { chat, onclose }: { chat: ChatStore; onclose: () => void } = $props();

  const goal = $derived(chat.goal);
  const statusView = $derived(goal ? goalState(goal.status) : null);
  const usage = $derived(goal ? goalUsage(goal) : '');
  const busy = $derived(controlBusy(`goal-clear:${chat.id}`));
  let armed = $state(false);

  async function stop() {
    if (!armed) {
      armed = true;
      return;
    }
    await stopGoal(chat.id);
    armed = false;
    onclose();
  }
</script>

<Sheet open title="Цель" subtitle={statusView?.text} {onclose}>
  {#if goal}
    <p class="objective" data-role="goal-objective">{goal.objective}</p>
    {#if usage}<p class="usage" data-role="goal-usage">{usage}</p>{/if}
    <button
      type="button"
      class="stop"
      data-role="goal-stop"
      aria-label="Остановить цель"
      aria-busy={busy}
      class:off={busy}
      onclick={() => { if (!busy) void stop(); }}
    >{armed ? 'Точно остановить?' : 'Остановить цель'}</button>
  {:else}
    <p>Цель не поставлена</p>
  {/if}
</Sheet>

<style>
  .objective {
    margin: 0;
    color: var(--text);
    font-size: var(--fs-body);
    line-height: var(--lh-body);
    overflow-wrap: anywhere;
  }
  .usage {
    margin: var(--s2) 0 0;
    color: var(--muted);
    font-size: var(--fs-sm);
    line-height: var(--lh-tight);
  }
  .stop { margin-top: var(--s4); min-height: var(--target); padding: var(--s2) var(--s3); border-radius: var(--radius-pill); white-space: nowrap; background: var(--raised); color: var(--danger); font-size: var(--fs-body); font-weight: var(--weight-medium); }
  .stop.off { color: var(--muted); }
  .stop:hover,
  .stop:focus-visible {
    background: var(--danger-soft);
  }
</style>
