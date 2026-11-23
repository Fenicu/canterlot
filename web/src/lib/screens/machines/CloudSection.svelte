<script lang="ts">
  import { onMount } from 'svelte';
  import { resolve } from '$app/paths';
  import {
    createCloudProposal,
    deleteCloudWindow,
    getCloudWindows
  } from '#lib/api/client.js';
  import { errorText } from '#lib/api/errors.js';
  import { ApiError } from '#lib/api/http.js';
  import type { Runtime } from '#lib/app/runtime.js';
  import { rubles } from '#lib/format/money.js';
  import { fmt } from '#lib/format/ru.js';
  import { cloudWindowView } from '#lib/requests/cloudWindowView.js';
  import {
    blockText,
    deleteOutcomeText,
    isLiveWindow,
    proposalRefusalText,
    splitWindows,
    windowAge,
    windowCost,
    windowCostLine,
    windowDeadline,
    windowErrorText,
    windowPriceLine,
    windowStateWord,
    type CloudWindow,
    type CloudWindows
  } from './cloudModel.js';
  import Button from '#lib/ui/Button.svelte';
  import ConfirmDialog from '#lib/ui/ConfirmDialog.svelte';
  import Icon from '#lib/ui/Icon.svelte';
  import Segmented from '#lib/ui/Segmented.svelte';

  let { runtime, now }: { runtime: Runtime; now: number } = $props();

  /** Живое окно меняется без событий: состояние и расход читаются опросом, пока оно идёт. */
  const POLL_MS = 15_000;
  const HOURS = [
    { value: '1', label: '1 ч' },
    { value: '2', label: '2 ч' },
    { value: '3', label: '3 ч' },
    { value: '4', label: '4 ч' }
  ];

  let list = $state<CloudWindows | null>(null);
  let error = $state('');
  let notice = $state<{ text: string; tone: 'ok' | 'warning' | 'error' } | null>(null);
  let pendingDelete = $state<CloudWindow | null>(null);
  let deleting = $state(false);
  let hours = $state('2');
  let reason = $state('');
  let proposing = $state(false);

  const split = $derived(list ? splitWindows(list.items) : { live: null, ended: [] });
  const block = $derived(list ? blockText(list.provider_block_code) : null);
  const openCards = $derived(runtime.list.cloudCards);

  async function load() {
    try {
      list = (await getCloudWindows()).data;
      error = '';
    } catch {
      error = 'Не удалось загрузить облачные окна. Повторите при восстановлении связи';
    }
  }
  onMount(() => { void load(); });

  // Пока окно живо, хаб сам ведёт его по состояниям: экран не ждёт события, которого нет.
  $effect(() => {
    if (!list?.items.some(isLiveWindow)) return;
    const timer = window.setInterval(() => void load(), POLL_MS);
    return () => window.clearInterval(timer);
  });

  async function confirmDelete() {
    const target = pendingDelete;
    if (!target || deleting) return;
    deleting = true;
    notice = null;
    try {
      const { data } = await deleteCloudWindow(target.id, crypto.randomUUID());
      notice = { text: deleteOutcomeText(data.outcome), tone: data.outcome === 'deleting' ? 'ok' : 'warning' };
      pendingDelete = null;
      await load();
    } catch (err) {
      notice = {
        text: errorText(err, 'Не удалось удалить окно. Повторите при восстановлении связи'),
        tone: 'error'
      };
    } finally {
      deleting = false;
    }
  }

  async function propose() {
    if (proposing) return;
    const text = reason.trim();
    if (!text) return;
    const parsed = Number(hours);
    proposing = true;
    notice = null;
    try {
      const { data } = await createCloudProposal({ hours: parsed, reason: text }, crypto.randomUUID());
      reason = '';
      notice = data.over_budget
        ? {
            text: `Карточка создана. Резерв ${rubles(data.reservation_kopecks)} больше остатка бюджета, поэтому карточка помечена «сверх бюджета»`,
            tone: 'warning'
          }
        : {
            text: `Карточка создана: резерв ${rubles(data.reservation_kopecks)}. Окно запустится только после вашего решения`,
            tone: 'ok'
          };
      // Карточка живёт в снимке списка, а не в событиях машин: баннер оболочки ждёт новый снимок.
      runtime.list.refresh();
      await load();
    } catch (err) {
      const raw = err instanceof ApiError ? (err.details as Record<string, unknown> | undefined)?.reason : undefined;
      const code = typeof raw === 'string' ? raw : '';
      notice = {
        text: code ? proposalRefusalText(code) : errorText(err, 'Не удалось создать карточку. Повторите при восстановлении связи'),
        tone: 'error'
      };
    } finally {
      proposing = false;
    }
  }
</script>

<section class="cloud" aria-labelledby="cloud-title" data-cloud-section>
  <h2 id="cloud-title">Облако</h2>
  {#if error && !list}
    <p class="error" role="alert">{error}</p>
    <button type="button" onclick={load}>Повторить</button>
  {:else if !list}
    <p class="note">Загружаю облачные окна…</p>
  {:else}
    {#if !list.configured}
      <p class="note" data-cloud-off>Облако не настроено на хабе: окна не создаются</p>
    {:else}
      {#if block}
        <p class="warning" role="status" data-cloud-block>{block}. Снимите блок в настройках</p>
      {/if}

      {#if split.live}
        {@const window = split.live}
        {@const problem = windowErrorText(window.last_error)}
        {@const deadline = windowDeadline(window, now)}
        <div class="window" data-cloud-window-card data-state={window.state}>
          <div class="window-head">
            <strong>{windowStateWord(window.state)}</strong>
            <span class="muted">{windowPriceLine(window)}</span>
          </div>
          <p class="muted">Окно живёт {windowAge(window, now)}</p>
          <p class="money">{windowCostLine(window)}</p>
          {#if deadline}<p class="muted">{deadline}</p>{/if}
          {#if problem}<p class="warning">{problem}</p>{/if}
          {#if window.state === 'deleting'}
            <p class="muted">Оплата идёт до подтверждения удаления</p>
          {:else}
            <div class="actions">
              <Button onclick={() => pendingDelete = window}>Удалить сейчас</Button>
            </div>
          {/if}
        </div>
      {:else}
        <p class="note" data-cloud-empty>Живого облачного окна нет</p>
      {/if}

      {#each openCards as card (card.id)}
        {@const view = cloudWindowView(card.request_payload)}
        <p class="note" data-cloud-card-line>
          Открыта карточка: {view.hoursText}, резерв {view.reservationText}. <a href={resolve('/c/[id]', { id: card.conversation_id })}>Открыть</a>
        </p>
      {/each}

      {#if split.ended.length > 0}
        <details class="history" data-cloud-history>
          <summary>
            <span>Законченные окна · {split.ended.length}</span>
            <span class="chevron" aria-hidden="true"><Icon name="chevron" /></span>
          </summary>
          <ul>
            {#each split.ended as window (window.id)}
              <li data-cloud-ended data-state={window.state}>
                <span class="ended-word">{windowStateWord(window.state)}</span>
                <span class="muted">{fmt.stamp(Date.parse(window.created_at), now)}</span>
                <strong>{rubles(windowCost(window))}</strong>
              </li>
            {/each}
          </ul>
        </details>
      {/if}

      {#if !block}
        <div class="form" data-cloud-form>
          <h3>Запустить облачный раннер</h3>
          <p class="note">Форма создаёт карточку, а не окно: сервер запустится только после вашего решения в карточке</p>
          <div class="field">
            <span class="label">Сколько часов</span>
            <Segmented name="cloud-hours" label="Сколько часов" options={HOURS} bind:value={hours} />
          </div>
          <label class="field">
            <span class="label">Причина</span>
            <textarea rows="3" bind:value={reason} placeholder="Например: прогнать гейт на облачном раннере"></textarea>
          </label>
          <div class="actions">
            {#if proposing}
              <Button variant="primary" disabled reason="Создаю карточку…">Создать карточку</Button>
            {:else if reason.trim()}
              <Button variant="primary" onclick={propose}>Создать карточку</Button>
            {:else}
              <Button variant="primary" disabled reason="Напишите причину: её увидите вы и агент">Создать карточку</Button>
            {/if}
          </div>
        </div>
      {/if}

      {#if notice}
        <p class="notice" data-tone={notice.tone} role={notice.tone === 'error' ? 'alert' : 'status'}>{notice.text}</p>
      {/if}
    {/if}
  {/if}
</section>

{#if pendingDelete}
  <ConfirmDialog
    title="Удалить облачное окно?"
    body={`Сервер удалится, окно закроется по фактической цене. Уже израсходовано ${rubles(windowCost(pendingDelete))}. Отменить нельзя.`}
    confirmLabel="Удалить сейчас"
    danger
    onconfirm={confirmDelete}
    onclose={() => pendingDelete = null}
  />
{/if}

<style>
  .cloud { margin-block: var(--s4) var(--s10); }
  .window,
  .form,
  .history {
    padding: var(--s6) var(--s8);
    margin-block: var(--s4);
    border-radius: var(--radius-card);
    background: var(--surface);
  }
  .window-head {
    display: flex;
    flex-wrap: wrap;
    gap: var(--s2) var(--s6);
    align-items: baseline;
    justify-content: space-between;
  }
  .window-head strong { font-size: var(--fs-body); }
  .money { margin-block: var(--s2) var(--s0); font-variant-numeric: tabular-nums; }
  .muted { margin-block: var(--s1); color: var(--muted); font-size: var(--fs-meta); }
  .warning { color: var(--warning); font-size: var(--fs-meta); }
  .error { color: var(--danger); }
  .form h3 { margin: var(--s0) var(--s2) var(--s2); font-size: var(--fs-body); font-weight: var(--weight-semibold); }
  .form .note { margin: var(--s0) var(--s2) var(--s4); }
  .field { display: grid; gap: var(--s2); margin-block: var(--s4); }
  .label { color: var(--secondary); font-size: var(--fs-meta); }
  textarea {
    width: 100%;
    min-height: var(--target);
    box-sizing: border-box;
    padding: var(--s5) var(--s8);
    border: 0;
    border-radius: var(--radius);
    background: var(--raised);
    color: inherit;
    font: inherit;
  }
  textarea:focus-visible { outline: var(--focus-width) solid var(--accent); }
  .actions { display: flex; flex-wrap: wrap; gap: var(--s4); margin-block: var(--s4) var(--s0); }
  .history summary {
    cursor: pointer;
    min-height: var(--target);
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--s4);
    color: var(--secondary);
    font-size: var(--fs-meta);
  }
  .history .chevron { display: inline-flex; color: var(--muted); transition: transform var(--duration-fast) var(--ease); }
  .history[open] .chevron { transform: rotate(90deg); }
  .history ul { display: flex; flex-direction: column; gap: var(--s2); margin: var(--s2) var(--s0) var(--s0); padding: var(--s0); list-style: none; }
  .history li {
    display: grid;
    grid-template-columns: minmax(0, auto) minmax(0, 1fr) auto;
    gap: var(--s2) var(--s6);
    align-items: baseline;
    padding-block: var(--s3);
    border-top: var(--hairline) solid var(--line);
  }
  .history strong { font-variant-numeric: tabular-nums; }
  .ended-word { color: var(--secondary); font-size: var(--fs-meta); }
  .notice { margin-block: var(--s4); font-size: var(--fs-meta); color: var(--secondary); }
  .notice[data-tone='warning'] { color: var(--warning); }
  .notice[data-tone='error'] { color: var(--danger); }
</style>
