<script lang="ts">
  import { deleteNudge, putNudge } from '#lib/api/client.js';
  import { errorText } from '#lib/api/errors.js';
  import { fmt } from '#lib/format/ru.js';
  import type { ChatStore, NudgeRule } from '#lib/stores/chat.svelte.js';
  import { nudgeCounter, nudgeStateText, signalLabels } from '#lib/timeline/nudge.js';
  import Button from '#lib/ui/Button.svelte';
  import ConfirmDialog from '#lib/ui/ConfirmDialog.svelte';
  import Sheet from '#lib/ui/Sheet.svelte';

  let { chat, onclose }: { chat: ChatStore; onclose: () => void } = $props();

  const rule = $derived(chat.nudge);
  const stateText = $derived(nudgeStateText(rule));
  const counter = $derived(nudgeCounter(rule));
  const paused = $derived(rule?.enabled === false || rule?.state === 'paused');
  const last = $derived(chat.nudgeEvents[0] ?? null);
  const signals = $derived(signalLabels(last?.signals));
  const setBy = $derived(
    rule?.set_by === 'owner' ? 'Задано владельцем' : 'Задано агентом',
  );
  let busy = $state(false);
  let error = $state('');
  let confirming = $state(false);

  const OUTCOME: Record<string, string> = {
    nudged: 'толчок',
    exhausted: 'сдался',
    marker_seen: 'маркер готовности',
    asked_in_text: 'вопрос текстом',
    cancelled_state_changed: 'состояние изменилось',
  };

  async function toggle(): Promise<void> {
    if (!rule || busy) return;
    busy = true;
    error = '';
    try {
      const response = await putNudge(
        chat.id,
        { paused: !paused },
        crypto.randomUUID(),
      );
      chat.nudge = response.data as NudgeRule;
    } catch (err) {
      error = errorText(err, 'Не удалось изменить автопинок');
    } finally {
      busy = false;
    }
  }

  async function clear(): Promise<void> {
    if (busy) return;
    busy = true;
    error = '';
    try {
      await deleteNudge(chat.id, crypto.randomUUID());
      chat.nudge = null;
      confirming = false;
      onclose();
    } catch (err) {
      error = errorText(err, 'Не удалось снять автопинок');
    } finally {
      busy = false;
    }
  }

  function stamp(at: string): string {
    const value = Date.parse(at);
    return Number.isFinite(value) ? fmt.clock(value) : at;
  }
</script>

<Sheet open title="Автопинок" {onclose}>
  <div class="nudge-sheet" data-role="nudge-sheet">
    {#if !rule}
      <p class="meta">Правила нет: сессию никто не толкает.</p>
    {:else}
      <p class="state" data-role="nudge-state">{stateText}</p>
      <p class="meta" data-role="nudge-counter">{counter} · подряд {rule.consecutive} из {rule.max_consecutive}</p>
      <p class="meta">{setBy}{rule.set_by_session ? ` · сессия ${rule.set_by_session.slice(0, 8)}` : ''}</p>
      <p class="meta">Простой {rule.idle_minutes} мин, с фоновой работой {rule.background_minutes} мин. Маркеры: {rule.done_markers.join(' или ')}.</p>
      {#if last}
        <p class="meta" data-role="nudge-last-signals">
          Последние сигналы: {signals.length > 0 ? signals.join(', ') : 'без сигналов'}
        </p>
      {:else}
        <p class="meta">Толчков ещё не было.</p>
      {/if}
      {#if chat.nudgeEvents.length > 0}
        <h3>История</h3>
        <ul class="history">
          {#each chat.nudgeEvents as event (event.id)}
            <li>
              <time datetime={event.at}>{stamp(event.at)}</time>
              <span>{OUTCOME[event.outcome] ?? event.outcome}</span>
              {#if signalLabels(event.signals).length > 0}
                <span class="signals">{signalLabels(event.signals).join(', ')}</span>
              {/if}
            </li>
          {/each}
        </ul>
      {/if}
      {#if error}<p class="error" role="alert">{error}</p>{/if}
      <div class="actions">
        {#if busy}
          <Button disabled reason="Подождите">{paused ? 'Возобновить' : 'Поставить на паузу'}</Button>
          <Button disabled reason="Подождите">Снять правило</Button>
        {:else}
          <Button onclick={() => void toggle()}>
            {paused ? 'Возобновить' : 'Поставить на паузу'}
          </Button>
          <Button onclick={() => (confirming = true)}>Снять правило</Button>
        {/if}
      </div>
    {/if}
  </div>
</Sheet>

{#if confirming}
  <ConfirmDialog
    title="Снять автопинок?"
    body="Исполнитель больше не получит автоматических толчков. Удержанный итог, если он ждёт, уйдёт подписчикам."
    confirmLabel="Снять автопинок"
    onconfirm={() => void clear()}
    onclose={() => (confirming = false)}
  />
{/if}

<style>
  .nudge-sheet {
    display: flex;
    flex-direction: column;
    gap: var(--s3);
  }
  .state {
    margin: 0;
    font-weight: var(--weight-semibold);
  }
  .meta {
    margin: 0;
    color: var(--muted);
    font-size: var(--fs-meta);
  }
  h3 {
    margin: var(--s3) 0 0;
    font-size: var(--fs-sm);
    color: var(--secondary);
  }
  .history {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--s2);
  }
  .history li {
    display: flex;
    flex-wrap: wrap;
    gap: var(--s2) var(--s4);
    color: var(--secondary);
    font-size: var(--fs-sm);
  }
  time {
    color: var(--time);
    font-variant-numeric: tabular-nums;
  }
  .signals { color: var(--muted); }
  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: var(--s3);
    margin-top: var(--s3);
  }
  .error {
    margin: 0;
    color: var(--danger);
  }
</style>
