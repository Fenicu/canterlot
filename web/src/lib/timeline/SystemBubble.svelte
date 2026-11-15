<script lang="ts">
  import { resolve } from '$app/paths';
  import { fmt } from '#lib/format/ru.js';
  import type { SystemView } from '#lib/timeline/system.js';
  import TextBlock from '#lib/timeline/TextBlock.svelte';
  import Button from '#lib/ui/Button.svelte';
  import Icon from '#lib/ui/Icon.svelte';
  import IconButton from '#lib/ui/IconButton.svelte';
  import { timeTip } from '#lib/ui/time-tip.js';

  let {
    view,
    createdAt,
    open,
    onToggle,
    onCompact,
    bookmark = null
  }: {
    view: SystemView;
    createdAt: number | null;
    open: boolean;
    onToggle: () => void;
    onCompact?: () => void;
    /** Закладка на блок пузыря: у ошибки API своя отметка, как у карточки ответа. */
    bookmark?: { marked: boolean; label: string; onToggle: () => void } | null;
  } = $props();

  let noteOpen = $state(false);
  const hasNote = $derived(Boolean(view.note));
</script>

<!-- Служебный пузырь: слева, как карточка итога делегата, с тем же набором фактов. -->
<article class="system" data-event-kind={view.kind} data-tone={view.tone} aria-label={view.title}>
  <header>
    <span class="icon"><Icon name={view.icon} /></span>
    {#if view.titleHref}
      <a class="title" href={resolve(view.titleHref as '/')}>{view.title}</a>
    {:else}
      <span class="title">{view.title}</span>
    {/if}
    {#if createdAt !== null || bookmark}
      <div class="meta">
        {#if createdAt !== null}
          <time datetime={new Date(createdAt).toISOString()} use:timeTip={createdAt}>{fmt.clock(createdAt)}</time>
        {/if}
        {#if bookmark}
          <div class="actions" data-role="bubble-actions">
            <IconButton
              icon="bookmark"
              solid={bookmark.marked}
              pressed={bookmark.marked}
              active={bookmark.marked}
              label={bookmark.label}
              onclick={bookmark.onToggle}
            />
          </div>
        {/if}
      </div>
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
  {/if}
  <!-- Действия карточки стоят одной строкой: у каждой цель касания var(--target), а зазор
       между ними не сходится внахлёст, как это делали отрицательные отступы в столбце. -->
  <div class="controls" data-role="system-controls">
    {#if hasNote}
      <button type="button" class="link-button" aria-expanded={noteOpen} onclick={() => { noteOpen = !noteOpen; }}>
        {noteOpen ? 'Скрыть заметку' : 'Показать заметку'}
      </button>
    {/if}
    {#if view.action}
      {#if view.action.kind === 'link'}
        {#if view.action.external}
          <!-- eslint-disable-next-line svelte/no-navigation-without-resolve -->
          <a class="action" href={view.action.href} target="_blank" rel="noopener noreferrer">{view.action.label}</a>
        {:else}
          <a class="action" href={resolve(view.action.href as '/')}>{view.action.label}</a>
        {/if}
      {:else}
        <Button onclick={() => onCompact?.()}>{view.action.label}</Button>
      {/if}
    {/if}
    <button type="button" class="text-toggle" aria-expanded={open} onclick={onToggle}>
      <span class="fold-label">{view.foldLabel ?? 'Текст для агента'}</span>
      <!-- Стрелка стоит после подписи, как в меню и плане: перед подписью она читалась
           разделителем между соседними действиями карточки. -->
      <span class="chevron" data-open={open ? '' : undefined}><Icon name="chevron" /></span>
    </button>
  </div>
  {#if open}
    <div class="fold">
      {#if view.plain}
        <p class="body plain" data-role="system-body">{view.text}</p>
      {:else}
        <div class="body" data-role="system-body">
          <TextBlock block={{ id: `system:${view.kind}:${view.title}`, text: view.text, state: 'final' }} />
        </div>
      {/if}
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
  .system[data-tone='danger'] { background: var(--danger-soft); }
  /* Заголовок, время и закладка стоят одной строкой сетки: длинный заголовок переносится
     в своей колонке, а пара справа остаётся в полосе его первой строки и не уезжает
     отдельной строкой под ним. */
  header {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr);
    align-items: baseline;
    gap: var(--s3) var(--s6);
    min-width: 0;
  }
  header:has(.meta) { grid-template-columns: auto minmax(0, 1fr) auto; }
  .icon {
    display: inline-flex;
    align-self: start;
    margin-block-start: calc((var(--fs-body) * var(--lh-body) - var(--icon-small)) / 2);
    color: var(--muted);
  }
  .icon :global(svg) { width: var(--icon-small); height: var(--icon-small); }
  [data-tone='warning'] .icon, [data-tone='warning'] .title { color: var(--warning); }
  [data-tone='danger'] .icon, [data-tone='danger'] .title { color: var(--danger); }
  .title {
    min-width: 0;
    color: var(--text);
    font-weight: var(--weight-semibold);
    overflow-wrap: anywhere;
  }
  .title[href] { display: inline-flex; align-items: center; min-height: var(--target); margin-block: calc((var(--target) - var(--fs-body) * var(--lh-body)) / -2); color: var(--accent-link); text-decoration: none; }
  .title[href]:hover, .title[href]:focus-visible { text-decoration: underline; text-underline-offset: var(--s1); }
  time { color: var(--time); font-size: var(--fs-sm); font-variant-numeric: tabular-nums; }
  /* Время и закладка ходят парой в правой колонке шапки. */
  .meta { display: flex; align-items: center; justify-self: end; gap: var(--s2); }
  .actions { display: flex; align-self: center; margin-block: calc((var(--target) - var(--fs-sm) * var(--lh-body)) / -2); }
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
  .controls { display: flex; flex-wrap: wrap; align-items: center; gap: var(--s4) var(--s6); }
  .link-button, .text-toggle {
    display: inline-flex;
    align-items: center;
    gap: var(--s3);
    min-height: var(--target);
    padding: var(--s0);
    background: var(--transparent);
    color: var(--accent-link);
    font-size: var(--fs-sm);
    text-align: left;
  }
  /* Раскрытие — ссылка карточки, как «Показать полностью» в пузыре сессии: заливка
     --raised на поверхности --surface превращала две соседние кнопки в одну фигуру. */
  .link-button:hover, .link-button:focus-visible,
  .text-toggle:hover, .text-toggle:focus-visible {
    background: var(--transparent);
    text-decoration: underline;
    text-underline-offset: var(--s1);
  }
  .link-button:active, .text-toggle:active { background: var(--transparent); }
  /* На мягкой заливке предупреждения и опасности accent-link не держит 4.5:1 в части светлых
     палитр: там раскрытие читается цветом текста карточки с подчёркиванием, как ссылка провайдера. */
  .system[data-tone='warning'] :is(.link-button, .text-toggle),
  .system[data-tone='danger'] :is(.link-button, .text-toggle) {
    color: var(--text);
    text-decoration: underline;
    text-underline-offset: var(--s1);
    text-decoration-color: var(--accent-link);
  }
  .chevron { display: inline-flex; color: var(--muted); transition: transform var(--duration-expand) var(--ease-expand); }
  .chevron :global(svg) { width: var(--icon-small); height: var(--icon-small); }
  .chevron[data-open] { transform: rotate(90deg); }
  .action { display: inline-flex; align-items: center; min-height: var(--target); color: var(--accent-link); font-size: var(--fs-sm); text-decoration: none; }
  .action:hover, .action:focus-visible { text-decoration: underline; text-underline-offset: var(--s1); }
  /* Ссылка провайдера на мягкой заливке: цвет текста держит 4.5:1 во всех палитрах,
     а подчёркивание оставляет её ссылкой. */
  .system[data-event-kind='api_error'] .action { color: var(--text); text-decoration: underline; text-underline-offset: var(--s1); text-decoration-color: var(--accent-link); }
  .fold { display: grid; grid-template-rows: 1fr; animation: fold var(--duration-expand) var(--ease-expand); }
  .body { min-height: 0; overflow: hidden; color: var(--secondary); }
  /* Исходный текст ошибки — вторичная подробность: как пояснение, а не как тело ответа. */
  .body.plain { margin: 0; padding-block: var(--s2); color: var(--secondary); font-size: var(--fs-sm); white-space: pre-wrap; overflow-wrap: anywhere; }
  @keyframes fold { from { grid-template-rows: 0fr; } to { grid-template-rows: 1fr; } }
</style>
