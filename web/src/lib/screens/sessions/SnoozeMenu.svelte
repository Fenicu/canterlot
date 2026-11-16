<script lang="ts">
  import Icon from '#lib/ui/Icon.svelte';
  import MenuItem from '#lib/ui/MenuItem.svelte';
  import { clearSnooze, snoozeSession } from '#lib/screens/chat/controls.js';
  import {
    choiceLabel,
    choiceUntil,
    snoozeChoices,
    type SnoozeChoice,
  } from './snooze';

  let {
    conversationId,
    snoozed,
    resetAt = null,
    now = Date.now(),
    inline = false,
    ondone,
    onerror
  }: {
    conversationId: string;
    /** Сессия отложена прямо сейчас: вместо выбора сроков предлагается «Вернуть». */
    snoozed: boolean;
    /** Срок сброса лимита, если у сессии есть пауза или удержание с известным временем. */
    resetAt?: number | null;
    now?: number;
    /** Компактный вид для строки листа: пункт меню занимал бы лишнюю высоту. */
    inline?: boolean;
    ondone?: () => void;
    onerror?: (error: unknown) => void;
  } = $props();

  let open = $state(false);
  let choosing = $state(false);
  let busy = $state(false);

  /** Значение `datetime-local` в местном времени без секунд. */
  function localValue(at: Date): string {
    const pad = (n: number) => String(n).padStart(2, '0');
    return `${at.getFullYear()}-${pad(at.getMonth() + 1)}-${pad(at.getDate())}T${pad(at.getHours())}:${pad(at.getMinutes())}`;
  }

  let custom = $state(localValue(new Date(Date.now() + 2 * 60 * 60_000)));

  const choices = $derived(snoozeChoices(now, resetAt));

  async function apply(until: Date | null): Promise<void> {
    if (!until || busy) return;
    busy = true;
    try {
      await snoozeSession(conversationId, until);
      open = false;
      choosing = false;
      ondone?.();
    } catch (error) {
      onerror?.(error);
    } finally {
      busy = false;
    }
  }

  async function returnNow(): Promise<void> {
    if (busy) return;
    busy = true;
    try {
      await clearSnooze(conversationId);
      ondone?.();
    } catch (error) {
      onerror?.(error);
    } finally {
      busy = false;
    }
  }

  function pick(choice: SnoozeChoice) {
    if (choice.kind === 'custom') {
      choosing = !choosing;
      return;
    }
    void apply(choiceUntil(choice));
  }

  function applyCustom() {
    const at = new Date(custom);
    if (!Number.isFinite(at.getTime())) return;
    void apply(at);
  }
</script>

{#snippet list()}
  <div class="choices" role="group" aria-label="Отложить до">
    {#each choices as choice (choiceLabel(choice))}
      <button
        type="button"
        role="menuitem"
        class="choice"
        onclick={() => pick(choice)}
      >
        {choiceLabel(choice)}
        {#if choice.kind === 'limit'}<Icon name="clock" />{/if}
      </button>
    {/each}
    {#if choosing}
      <div class="custom">
        <input
          type="datetime-local"
          aria-label="Время возврата"
          bind:value={custom}
          min={localValue(new Date(now))}
          max={localValue(new Date(now + 30 * 24 * 60 * 60_000))}
        />
        <button type="button" class="confirm" onclick={applyCustom}>Отложить</button>
      </div>
    {/if}
  </div>
{/snippet}

{#if inline}
  <div class="inline-wrap">
    {#if snoozed}
      <button type="button" class="inline" onclick={() => void returnNow()}>Вернуть</button>
    {:else}
      <button
        type="button"
        class="inline"
        aria-expanded={open}
        onclick={() => { open = !open; choosing = false; }}
      >Отложить</button>
      {#if open}{@render list()}{/if}
    {/if}
  </div>
{:else if snoozed}
  <MenuItem icon="clock" onclick={() => void returnNow()}>Вернуть</MenuItem>
{:else}
  <MenuItem icon="clock" onclick={() => { open = !open; choosing = false; }}>Отложить</MenuItem>
  {#if open}{@render list()}{/if}
{/if}

<style>
  .choices {
    display: flex;
    flex-direction: column;
    padding: 0 var(--s3) var(--s2) var(--s9);
  }
  .choice {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--s3);
    min-height: var(--target);
    padding: var(--s2) var(--s4);
    border: var(--s0);
    border-radius: var(--radius-sm);
    background: var(--transparent);
    color: var(--text);
    font-size: var(--fs-body);
    text-align: left;
  }
  .choice :global(svg) { width: var(--icon-small); height: var(--icon-small); color: var(--muted); }
  @media (hover: hover) and (pointer: fine) {
    .choice:hover, .confirm:hover { background: var(--popover-hover); }
  }
  .custom {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--s3);
    padding: var(--s2) var(--s4);
  }
  input {
    flex: 1 1 var(--gauge-width);
    min-height: var(--target);
    padding: var(--s2) var(--s3);
    border: var(--s1) solid var(--line);
    border-radius: var(--radius-sm);
    background: var(--surface);
    color: var(--text);
    font-size: var(--fs-meta);
  }
  .confirm {
    min-height: var(--target);
    padding: var(--s2) var(--s4);
    border-radius: var(--radius-sm);
    background: var(--raised);
    color: var(--text);
  }
  .inline-wrap { display: flex; flex-direction: column; min-width: 0; }
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
  .inline-wrap .choices { padding: var(--s1) 0 0; }
  .inline-wrap .choice { padding-inline: var(--s3); }
</style>
