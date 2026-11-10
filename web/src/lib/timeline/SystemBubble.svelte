<script lang="ts">
  import { resolve } from '$app/paths';
  import { fmt } from '#lib/format/ru.js';
  import type { SystemView } from '#lib/timeline/system.js';
  import TextBlock from '#lib/timeline/TextBlock.svelte';
  import Button from '#lib/ui/Button.svelte';
  import Icon from '#lib/ui/Icon.svelte';
  import { timeTip } from '#lib/ui/time-tip.js';

  let {
    view,
    createdAt,
    open,
    onToggle,
    onCompact
  }: {
    view: SystemView;
    createdAt: number | null;
    open: boolean;
    onToggle: () => void;
    onCompact?: () => void;
  } = $props();

  let noteOpen = $state(false);
  const hasNote = $derived(Boolean(view.note));
</script>

<!-- Служебный пузырь: слева, как карточка итога делегата, с тем же набором фактов. -->
<article class="system" data-event-kind={view.kind} data-tone={view.tone}>
  <header>
    <span class="icon"><Icon name={view.icon} /></span>
    {#if view.titleHref}
      <a class="title" href={resolve(view.titleHref as '/')}>{view.title}</a>
    {:else}
      <span class="title">{view.title}</span>
    {/if}
    {#if createdAt !== null}
      <time datetime={new Date(createdAt).toISOString()} use:timeTip={createdAt}>{fmt.clock(createdAt)}</time>
    {/if}
  </header>
  {#if view.reason}<p class="reason">{view.reason}</p>{/if}
  {#if view.facts.length > 0}
    <dl class="facts">
      {#each view.facts as fact, index (`${fact.label}:${fact.value}:${index}`)}
        <div><dt>{fact.label}</dt><dd>{fact.value}</dd></div>
      {/each}
    </dl>
  {/if}
  {#if view.rows.length > 0}
    <ul class="rows">
      {#each view.rows as row, index (`${row.title}:${row.href ?? ''}:${index}`)}
        <li>
          {#if row.href}
            <a href={resolve(row.href as '/')}>{row.title}</a>
          {:else}
            <span class="row-title">{row.title}</span>
          {/if}
          {#if row.value}<span class="row-value">{row.value}</span>{/if}
        </li>
      {/each}
    </ul>
  {/if}
  {#if hasNote}
    <p class="note" class:clamped={!noteOpen} data-role="system-note">{view.note}</p>
    <button type="button" class="link-button" aria-expanded={noteOpen} onclick={() => { noteOpen = !noteOpen; }}>
      {noteOpen ? 'Скрыть заметку' : 'Показать заметку'}
    </button>
  {/if}
  {#if view.action}
    {#if view.action.kind === 'link'}
      <a class="action" href={resolve(view.action.href as '/')}>{view.action.label}</a>
    {:else}
      <Button onclick={() => onCompact?.()}>{view.action.label}</Button>
    {/if}
  {/if}
  <button type="button" class="text-toggle" aria-expanded={open} onclick={onToggle}>
    <span class="chevron" data-open={open ? '' : undefined}><Icon name="chevron" /></span>
    Текст для агента
  </button>
  {#if open}
    <div class="fold">
      <div class="body" data-role="system-body">
        <TextBlock block={{ id: `system:${view.kind}:${view.title}`, text: view.text, state: 'final' }} />
      </div>
    </div>
  {/if}
</article>

<style>
  .system {
    display: flex;
    flex-direction: column;
    gap: var(--s4);
    min-width: 0;
    padding: var(--s5) var(--s6);
    border-radius: var(--radius-card);
    background: var(--surface);
  }
  .system[data-tone='warning'] { background: var(--warning-soft); }
  header {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: var(--s3) var(--s6);
    min-width: 0;
  }
  .icon { display: inline-flex; align-self: center; color: var(--muted); }
  .icon :global(svg) { width: var(--icon-small); height: var(--icon-small); }
  [data-tone='warning'] .icon, [data-tone='warning'] .title { color: var(--warning); }
  .title {
    min-width: 0;
    color: var(--text);
    font-weight: var(--weight-semibold);
    overflow-wrap: anywhere;
  }
  .title[href] { display: inline-flex; align-items: center; min-height: var(--target); margin-block: calc((var(--target) - var(--fs-body) * var(--lh-body)) / -2); color: var(--accent-link); text-decoration: none; }
  .title[href]:hover, .title[href]:focus-visible { text-decoration: underline; text-underline-offset: var(--s1); }
  time { margin-left: auto; color: var(--time); font-size: var(--fs-sm); font-variant-numeric: tabular-nums; }
  .reason { margin: 0; color: var(--secondary); font-size: var(--fs-sm); overflow-wrap: anywhere; }
  .facts {
    display: grid;
    grid-template-columns: max-content minmax(0, 1fr);
    align-items: baseline;
    gap: var(--s2) var(--s6);
    margin: 0;
  }
  .facts div { display: contents; }
  dt { color: var(--muted); font-size: var(--fs-sm); }
  dd { margin: 0; color: var(--text); font-size: var(--fs-sm); font-variant-numeric: tabular-nums; overflow-wrap: anywhere; }
  .rows { display: flex; flex-direction: column; gap: var(--s2); margin: 0; padding: 0; list-style: none; }
  .rows li { display: flex; flex-wrap: wrap; align-items: baseline; gap: var(--s2) var(--s6); min-width: 0; }
  .rows a, .row-title { min-width: 0; color: var(--text); font-size: var(--fs-sm); overflow-wrap: anywhere; }
  .rows a { display: inline-flex; align-items: center; min-height: var(--target); margin-block: calc((var(--target) - var(--fs-sm) * var(--lh-body)) / -2); color: var(--accent-link); text-decoration: none; }
  .rows a:hover, .rows a:focus-visible { text-decoration: underline; text-underline-offset: var(--s1); }
  .row-value { color: var(--secondary); font-size: var(--fs-sm); font-variant-numeric: tabular-nums; }
  .note { margin: 0; color: var(--secondary); font-size: var(--fs-sm); overflow-wrap: anywhere; white-space: pre-wrap; }
  .note.clamped { display: -webkit-box; -webkit-box-orient: vertical; -webkit-line-clamp: 2; line-clamp: 2; overflow: hidden; }
  .link-button, .text-toggle {
    display: inline-flex;
    align-items: center;
    gap: var(--s3);
    min-height: var(--target);
    color: var(--accent-link);
    font-size: var(--fs-sm);
    text-align: left;
  }
  .link-button { align-self: flex-start; margin-block: calc(var(--s4) * -1); }
  .text-toggle { align-self: flex-start; margin-block: calc(var(--s4) * -1); }
  .chevron { display: inline-flex; color: var(--muted); transition: transform var(--duration-expand) var(--ease-expand); }
  .chevron :global(svg) { width: var(--icon-small); height: var(--icon-small); }
  .chevron[data-open] { transform: rotate(90deg); }
  .action { display: inline-flex; align-items: center; align-self: flex-start; min-height: var(--target); color: var(--accent-link); font-size: var(--fs-sm); text-decoration: none; }
  .action:hover, .action:focus-visible { text-decoration: underline; text-underline-offset: var(--s1); }
  .fold { display: grid; grid-template-rows: 1fr; animation: fold var(--duration-expand) var(--ease-expand); }
  .body { min-height: 0; overflow: hidden; color: var(--secondary); }
  @keyframes fold { from { grid-template-rows: 0fr; } to { grid-template-rows: 1fr; } }
</style>
