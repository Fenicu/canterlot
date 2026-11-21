<script lang="ts">
  import { untrack, type Snippet } from 'svelte';
  import { durationMs } from '#lib/motion.js';
  import { blockFold } from '#lib/ui/fold.js';
  import Icon from '#lib/ui/Icon.svelte';
  import InkIcon from '#lib/ui/InkIcon.svelte';
  import type { InkState } from '#lib/ui/ink.js';
  import { swapOn } from '#lib/ui/swap.js';
  import { arrivedNow } from '#lib/timeline/arrival.js';
  import ToolOutput from '#lib/timeline/ToolOutput.svelte';
  import { callCaption } from '#lib/timeline/present.js';
  import type { Tone } from './copy';
  import type { CardCall, CardEnv, CardState } from './types';

  let {
    call,
    env,
    line,
    status = '',
    tone = 'muted',
    quiet = false,
    grows = false,
    dot = false,
    live = false,
    head,
    children,
    fold,
    badge,
    reveal = 'вызов'
  }: {
    call: CardCall;
    env: CardEnv;
    /** Строка шапки; со своей шапкой — подпись кнопки раскрытия. */
    line: string;
    status?: string;
    tone?: Tone;
    quiet?: boolean;
    /** Карточка пришла на глазах: вырастает из края, как ветка делегирования. */
    grows?: boolean;
    /** Точка состояния перед словом: у сессий, как в списке сессий. */
    dot?: boolean;
    live?: boolean;
    head?: Snippet;
    children?: Snippet;
    /** Отметка рядом со словом состояния: фоновая команда, политика CLI. */
    badge?: Snippet;
    /** Своё раскрытие вместо сырого вызова: вывод команды, разница файла. */
    fold?: Snippet;
    /** Что раскрывает шеврон, в винительном: «вызов», «вывод», «изменения». */
    reveal?: string;
  } = $props();

  const called = $derived(call.block.text.trim().length > 0 && call.block.state !== 'streaming');

  // Значок семейства стоит на месте шеврона и сам рассказывает о состоянии вызова: набросок, пока он идёт,
  // росчерк один раз, когда он закончился на глазах, и ровный набросок при отказе.
  const icon = $derived(quiet ? null : env.icon);
  const inks = (): boolean => durationMs('--duration-ink') > 0;
  const resting = (state: CardState): InkState => (state === 'running' ? 'sketch' : state === 'done' ? 'dry' : 'refused');
  // Чернила идут только у вызова, пришедшего или завершившегося на глазах: история и строка, вернувшаяся в окно ленты, стоят сразу.
  let phase = $state<InkState>(untrack(() => (call.state === 'done' && env.icon && arrivedNow(`mark:${call.block.id}`, call.block.createdAt) && inks() ? 'ink' : resting(call.state))));
  let was = untrack(() => call.state);
  $effect(() => {
    const state = call.state;
    if (state === was) return;
    const before = was;
    was = state;
    phase = before === 'running' && state === 'done' && inks() ? 'ink' : resting(state);
  });
  // Когда росчерк и оседание цвета прошли, значок стоит как у любого давно законченного вызова.
  function dried(event: AnimationEvent): void {
    if (phase === 'ink' && event.animationName.endsWith('mark-settle')) phase = 'dry';
  }

  // Слово состояния меняется подписью, когда сменилось состояние; число внутри («12 мин») идёт на месте, без движения.
  const wordKey = $derived(`${tone}:${status.replace(/\d+/g, '#')}`);
</script>

<div class="feed-card" data-card={call.id} data-state={call.state} data-quiet={quiet ? '' : undefined} data-grows={grows ? '' : undefined} data-icon={icon ?? undefined}>
  <div class="head">
    <!-- Кнопка лежит под всей шапкой: ссылки в шапке стоят поверх неё и остаются ссылками. -->
    <button type="button" class="hit" aria-expanded={env.open} aria-label={`${line}. ${env.open ? 'Скрыть' : 'Показать'} ${reveal}`} onclick={env.onToggle}></button>
    {#if icon}<span class="family" data-role="card-icon" onanimationend={dried}><InkIcon name={icon} state={phase} breathe={!dot} /></span>{/if}
    <span class="chevron" data-open={env.open ? '' : undefined} aria-hidden="true"><Icon name="chevron" /></span>
    <span class="lead">
      {#if head}{@render head()}{:else}<span class="line">{line}</span>{/if}
    </span>
    {#if badge}{@render badge()}{/if}
    {#if dot && status}<span class="dot" data-tone={tone} data-live={live ? '' : undefined} aria-hidden="true"></span>{/if}
    <span class="word" data-tone={tone} data-role="card-state" use:swapOn={{ text: status, key: wordKey }}></span>
  </div>
  {#if children}<div class="content">{@render children()}</div>{/if}
  {#if env.open}
    <div class="fold" transition:blockFold>
      {#if fold}
        <div class="body" data-role="card-fold">{@render fold()}</div>
      {:else}
        <div class="body" data-role="card-raw">
          {#if called}<ToolOutput block={call.block} caption={callCaption(call.block)} load={env.load} />{/if}
          {#if call.result || !called}<ToolOutput block={call.result ?? call.block} running={call.state === 'running'} load={env.load} />{/if}
        </div>
      {/if}
    </div>
  {/if}
</div>

<style>
  .feed-card { display: flex; flex-direction: column; min-width: 0; border-radius: var(--radius-card); background: var(--raised); }
  .feed-card[data-quiet] { background: var(--transparent); }
  .head {
    position: relative;
    display: flex;
    align-items: center;
    gap: var(--s3);
    min-width: 0;
    min-height: var(--target);
    padding: var(--s2) var(--s5) var(--s2) var(--s4);
    font-size: var(--fs-sm);
  }
  .hit { position: absolute; inset: var(--s0); width: 100%; border-radius: var(--radius-card); background: var(--transparent); }
  .head :global(a) { position: relative; z-index: 1; }
  .chevron { flex: none; display: inline-flex; color: var(--muted); pointer-events: none; transition: transform var(--duration-expand) var(--ease-expand); }
  .chevron :global(svg) { width: var(--icon-small); height: var(--icon-small); }
  .chevron[data-open] { transform: rotate(90deg); }
  /* Значок семейства встаёт на место шеврона, шеврон уходит к правому краю: колонка текста остаётся той же, что у карточек без значка. */
  .family { flex: none; display: inline-flex; pointer-events: none; }
  .feed-card[data-icon] .chevron { order: 5; }
  .lead { display: flex; flex: 1 1 auto; align-items: baseline; gap: var(--s3); min-width: 0; pointer-events: none; }
  .lead :global(a) { pointer-events: auto; }
  .line { min-width: 0; overflow: hidden; color: var(--text); text-overflow: ellipsis; white-space: nowrap; }
  .feed-card[data-quiet] .line { color: var(--muted); }
  .word, .dot { color: var(--secondary); transition: color var(--duration-fast) var(--ease); }
  .word { flex: none; pointer-events: none; white-space: nowrap; }
  /* Точка стоит перед словом отдельным элементом, а зазор до слова даёт шапка: слово меняется подписью и уходит из документа, когда опустело. */
  .dot { flex: none; width: var(--mark); height: var(--mark); border-radius: var(--radius-round); background: currentColor; }
  /* Сессия занята делом: точка дышит, как активный пункт плана, и это единственное дыхание карточки: значок рядом стоит ровно. Без движения — ровный тон. */
  .dot[data-live] { animation: card-pulse var(--duration-stage-pulse) var(--ease-stage-pulse) infinite; }
  @keyframes card-pulse { 50% { opacity: var(--opacity-stage-halo); } }
  :is(.word, .dot)[data-tone='accent'] { color: var(--accent-link); }
  :is(.word, .dot)[data-tone='blue'] { color: var(--blue); }
  :is(.word, .dot)[data-tone='success'] { color: var(--success); }
  :is(.word, .dot)[data-tone='warning'] { color: var(--warning); }
  :is(.word, .dot)[data-tone='danger'] { color: var(--danger); }
  :is(.word, .dot)[data-tone='muted'] { color: var(--muted); }
  .content { display: flex; flex-direction: column; gap: var(--s2); min-width: 0; padding: var(--s0) var(--s5) var(--s4) calc(var(--s4) + var(--icon-small) + var(--s3)); font-size: var(--fs-sm); }
  /* Карточка передала пустой набор: тело без единого элемента не добавляет 8 px под шапкой. */
  .content:not(:has(> *)) { display: none; }
  .fold { display: grid; grid-template-rows: 1fr; }
  /* Тело раскрытой карточки встаёт на ту же колонку, что и шапка, — вывод не гуляет влево. */
  .body { display: flex; flex-direction: column; gap: var(--s2); min-height: 0; overflow: hidden; padding-inline: var(--s4); }
  .body > :global(:last-child) { margin-bottom: var(--s2); }
  /* Вырастает слева направо, как строка субагента: высота не меняется, лента не перемеряет соседей. */
  .feed-card[data-grows] { animation: card-reveal var(--duration-branch-grow) var(--ease-branch-grow) backwards; }
  @keyframes card-reveal {
    from { clip-path: inset(var(--s0) calc(100% - var(--s4) - var(--icon-small) - var(--s3)) var(--s0) var(--s0) round var(--radius-card)); }
    to { clip-path: inset(var(--s0) var(--s0) var(--s0) var(--s0) round var(--radius-card)); }
  }
  @media (prefers-reduced-motion: reduce) {
    .feed-card[data-grows], .dot[data-live] { animation: none; }
  }
</style>
