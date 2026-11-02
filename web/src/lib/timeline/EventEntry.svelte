<script lang="ts">
  import { resolve } from '$app/paths';
  import type { InputView } from '#lib/stores/inputs.js';
  import TextBlock from '#lib/timeline/TextBlock.svelte';
  import {
    LEGACY_CAPTION,
    collapsedText,
    eventFacts,
    eventLine,
    eventSessionId,
    eventTitle,
    isLongText,
    isSessionEvent,
    sessionStatus
  } from '#lib/timeline/hubEvent.js';

  let { input, open, onToggle }: { input: InputView; open: boolean; onToggle: () => void } = $props();

  const origin = $derived(input.origin);
  const event = $derived(origin?.event ?? null);
  const title = $derived(eventTitle(origin));
  const link = $derived(eventSessionId(origin));
  const rawTitle = $derived(origin?.title?.trim() ?? '');
  const session = $derived(isSessionEvent(event));
  const line = $derived(event && !session ? eventLine(event, rawTitle) : null);
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
    {#if answer}
      <button type="button" class="answer-toggle" aria-expanded={open} onclick={onToggle}>
        {open ? 'Скрыть ответ' : 'Показать ответ'}
      </button>
      {#if open}
        <div class="answer"><TextBlock block={{ id: input.key, text: answer, state: 'final' }} /></div>
      {/if}
    {/if}
  </article>
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
  .facts {
    display: flex;
    flex-wrap: wrap;
    gap: var(--s2) var(--s6);
    margin: 0;
  }
  .facts div { display: flex; align-items: baseline; gap: var(--s2); min-width: 0; }
  dt { color: var(--muted); font-size: var(--fs-sm); }
  dd {
    margin: 0;
    color: var(--secondary);
    font-size: var(--fs-sm);
    font-variant-numeric: tabular-nums;
    overflow-wrap: anywhere;
    white-space: pre-wrap;
  }
  .answer-toggle {
    align-self: flex-start;
    min-height: var(--target);
    padding-inline: var(--s0);
    color: var(--accent-link);
    font-size: var(--fs-sm);
  }
  .answer { min-width: 0; }
  .line {
    margin: 0;
    padding-inline: var(--s6);
    color: var(--secondary);
    font-size: var(--fs-sm);
    overflow-wrap: anywhere;
  }
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
