<script lang="ts">
  import { resolve } from '$app/paths';
  import { fmt } from '#lib/format/ru.js';
  import type { InputView } from '#lib/stores/inputs.js';
  import TextBlock from '#lib/timeline/TextBlock.svelte';
  import {
    LEGACY_CAPTION,
    alarmEntryLine,
    collapsedText,
    eventFacts,
    eventLine,
    eventSessionId,
    eventTitle,
    isLongText,
    isSessionEvent,
    sessionStatus
  } from '#lib/timeline/hubEvent.js';
  import { nudgeEntryLine, nudgeExhaustedLine } from '#lib/timeline/nudge.js';
  import Icon from '#lib/ui/Icon.svelte';

  let { input, open, onToggle }: { input: InputView; open: boolean; onToggle: () => void } = $props();

  const origin = $derived(input.origin);
  const event = $derived(origin?.event ?? null);
  const title = $derived(eventTitle(origin));
  const link = $derived(eventSessionId(origin));
  const rawTitle = $derived(origin?.title?.trim() ?? '');
  const session = $derived(isSessionEvent(event));
  // Строка будильника считается только для его ввода: у остальных записей текст доставки не читается.
  const alarm = $derived(origin?.kind === 'alarm' ? alarmEntryLine(origin, input.text) : null);
  const nudge = $derived(origin?.kind === 'nudge' ? nudgeEntryLine(origin) : null);
  // Подпись события не зависит от строки будильника: правка плашки не пересчитывает её.
  const line = $derived(
    event && !session && origin?.kind !== 'alarm' && origin?.kind !== 'nudge'
      ? eventLine(event, rawTitle)
      : null
  );
  const exhausted = $derived(event ? nudgeExhaustedLine(event) : null);
  const facts = $derived(event ? eventFacts(event) : []);
  const answer = $derived(event?.answer ?? '');
  const long = $derived(isLongText(input.text));
</script>

{#if session && event}
  <article class="card" data-event-kind={event.kind} data-event-state={event.state}>
    <header>
      {#if link}
        <a class="title" href={resolve(`/c/${link}` as '/')}>{title}</a>
      {:else}
        <span class="title">{title}</span>
      {/if}
      <span class="status" data-state={event.state}>{sessionStatus(event)}</span>
    </header>
    {#if facts.length > 0}
      <dl class="facts">
        {#each facts as fact (fact.label)}
          <div><dt>{fact.label}</dt><dd>{fact.value}</dd></div>
        {/each}
      </dl>
    {/if}
    {#if exhausted}
      <p class="line nudge-exhausted" data-role="nudge-exhausted">{exhausted}</p>
    {/if}
    {#if answer}
      <button type="button" class="answer-toggle" aria-expanded={open} onclick={onToggle}>
        {open ? 'Скрыть ответ' : 'Показать ответ'}
      </button>
      {#if open}
        <div class="answer"><TextBlock block={{ id: input.key, text: answer, state: 'final' }} /></div>
      {/if}
    {/if}
    {#if input.createdAt !== null}
      <footer class="stamp" data-role="event-footer">
        <time datetime={new Date(input.createdAt).toISOString()}>{fmt.clock(input.createdAt)}</time>
      </footer>
    {/if}
  </article>
{:else if alarm}
  <div class="alarm" data-role="alarm">
    <button type="button" class="alarm-head" aria-expanded={open} onclick={onToggle}>
      <span class="chevron" data-open={open ? '' : undefined}><Icon name="chevron" /></span>
      <span class="line" data-event-kind="alarm">{alarm}</span>
    </button>
    {#if open}
      <div class="fold">
        <div class="alarm-body" data-role="alarm-body">
          <TextBlock block={{ id: input.key, text: input.text, state: 'final' }} />
        </div>
      </div>
    {/if}
  </div>
{:else if nudge}
  <div class="alarm" data-role="nudge-line" data-event-kind="nudge">
    <button type="button" class="alarm-head" aria-expanded={open} onclick={onToggle}>
      <span class="chevron" data-open={open ? '' : undefined}><Icon name="chevron" /></span>
      <span class="line">{nudge}</span>
    </button>
    {#if open}
      <div class="fold">
        <div class="alarm-body" data-role="nudge-body">
          <TextBlock block={{ id: input.key, text: input.text, state: 'final' }} />
        </div>
      </div>
    {/if}
  </div>
{:else if line}
  <p class="line" data-event-kind={event?.kind}>{line}</p>
{:else}
  <article class="legacy" data-event-kind="legacy">
    <p class="caption">{LEGACY_CAPTION}</p>
    <p class="text">{open || !long ? input.text : collapsedText(input.text)}</p>
    {#if long}
      <button type="button" class="answer-toggle" aria-expanded={open} onclick={onToggle}>
        {open ? 'Скрыть текст' : 'Показать текст'}
      </button>
    {/if}
  </article>
{/if}

<style>
  .card {
    display: flex;
    flex-direction: column;
    gap: var(--s4);
    min-width: 0;
    padding: var(--s5) var(--s6);
    border-radius: var(--radius-card);
    background: var(--surface);
  }
  header {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: var(--s3) var(--s6);
    min-width: 0;
  }
  .title {
    min-width: 0;
    color: var(--text);
    font-weight: var(--weight-semibold);
    overflow-wrap: anywhere;
  }
  .title[href] { color: var(--accent-link); text-decoration: none; }
  .title[href]:hover, .title[href]:focus-visible { text-decoration: underline; text-underline-offset: var(--s1); }
  .status { color: var(--secondary); font-size: var(--fs-sm); white-space: nowrap; }
  .status[data-state='idle'] { color: var(--success); }
  .status[data-state='waiting_for_user'] { color: var(--accent-link); }
  .status[data-state='stalled'], .status[data-state='quota_paused'] { color: var(--warning); }
  .status[data-state='failed'] { color: var(--danger); }
  /* Подписи в своей колонке, значения с одного края: пары не сливаются в одно предложение. */
  .facts {
    display: grid;
    grid-template-columns: max-content minmax(0, 1fr);
    align-items: baseline;
    gap: var(--s2) var(--s6);
    margin: 0;
  }
  .facts div { display: contents; }
  dt { color: var(--muted); font-size: var(--fs-sm); }
  dd {
    margin: 0;
    color: var(--text);
    font-size: var(--fs-sm);
    font-variant-numeric: tabular-nums;
    overflow-wrap: anywhere;
    white-space: pre-wrap;
  }
  .answer-toggle {
    align-self: flex-start;
    margin-top: var(--s4);
    min-height: var(--target);
    padding-inline: var(--s6);
    color: var(--accent-link);
    font-size: var(--fs-sm);
  }
  .answer { min-width: 0; }
  .stamp { color: var(--time); font-size: var(--fs-sm); font-variant-numeric: tabular-nums; }
  .alarm { display: flex; flex-direction: column; min-width: 0; }
  .alarm-head {
    display: flex;
    gap: var(--s3);
    align-items: center;
    width: 100%;
    min-height: var(--target);
    padding: var(--s0) var(--s6) var(--s0) var(--s4);
    text-align: left;
  }
  .chevron {
    flex: none;
    color: var(--muted);
    transition: transform var(--duration-expand) var(--ease-expand);
  }
  .chevron :global(svg) { width: var(--icon-small); height: var(--icon-small); }
  .chevron[data-open] { transform: rotate(90deg); }
  .fold { display: grid; grid-template-rows: 1fr; animation: fold var(--duration-expand) var(--ease-expand); }
  .alarm-body { min-height: 0; overflow: hidden; padding: var(--s2) var(--s6) var(--s4); color: var(--secondary); }
  @keyframes fold { from { grid-template-rows: 0fr; } to { grid-template-rows: 1fr; } }
  .line {
    margin: 0;
    padding-inline: var(--s6);
    color: var(--secondary);
    font-size: var(--fs-sm);
    overflow-wrap: anywhere;
  }
  .alarm-head .line { padding-inline: 0; }
  .legacy {
    display: flex;
    flex-direction: column;
    gap: var(--s2);
    min-width: 0;
    padding: var(--s4) var(--s6);
    border-radius: var(--radius-sm);
    background: var(--raised);
  }
  .caption { margin: 0; color: var(--muted); font-size: var(--fs-sm); font-weight: var(--weight-medium); }
  .text { margin: 0; color: var(--secondary); overflow-wrap: anywhere; white-space: pre-wrap; }
</style>
