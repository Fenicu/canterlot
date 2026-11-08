<script lang="ts">
  import type { Snippet } from 'svelte';
  import { blockFold } from '#lib/ui/fold.js';
  import Icon from '#lib/ui/Icon.svelte';
  import ToolOutput from '#lib/timeline/ToolOutput.svelte';
  import { callCaption } from '#lib/timeline/present.js';
  import type { Tone } from './copy';
  import type { CardCall, CardEnv } from './types';

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
    children
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
  } = $props();

  const called = $derived(call.block.text.trim().length > 0 && call.block.state !== 'streaming');
</script>

<div class="feed-card" data-card={call.id} data-state={call.state} data-quiet={quiet ? '' : undefined} data-grows={grows ? '' : undefined}>
  <div class="head">
    <!-- Кнопка лежит под всей шапкой: ссылки в шапке стоят поверх неё и остаются ссылками. -->
    <button type="button" class="hit" aria-expanded={env.open} aria-label={`${line}. ${env.open ? 'Скрыть вызов' : 'Показать вызов'}`} onclick={env.onToggle}></button>
    <span class="chevron" data-open={env.open ? '' : undefined} aria-hidden="true"><Icon name="chevron" /></span>
    <span class="lead">
      {#if head}{@render head()}{:else}<span class="line">{line}</span>{/if}
    </span>
    {#if status}<span class="word" data-tone={tone} data-role="card-state">{#if dot}<span class="dot" data-live={live ? '' : undefined} aria-hidden="true"></span>{/if}{status}</span>{/if}
  </div>
  {#if children}<div class="content">{@render children()}</div>{/if}
  {#if env.open}
    <div class="fold" transition:blockFold>
      <div class="body" data-role="card-raw">
        {#if called}<ToolOutput block={call.block} caption={callCaption(call.block)} load={env.load} />{/if}
        {#if call.result || !called}<ToolOutput block={call.result ?? call.block} running={call.state === 'running'} load={env.load} />{/if}
      </div>
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
  .lead { display: flex; flex: 1 1 auto; align-items: baseline; gap: var(--s3); min-width: 0; pointer-events: none; }
  .lead :global(a) { pointer-events: auto; }
  .line { min-width: 0; overflow: hidden; color: var(--text); text-overflow: ellipsis; white-space: nowrap; }
  .feed-card[data-quiet] .line { color: var(--muted); }
  .word { flex: none; color: var(--secondary); pointer-events: none; white-space: nowrap; transition: color var(--duration-fast) var(--ease); }
  .word[data-tone='accent'] { color: var(--accent-link); }
  .dot { display: inline-block; width: var(--mark); height: var(--mark); margin-inline-end: var(--s3); border-radius: var(--radius-round); background: currentColor; vertical-align: middle; transition: background-color var(--duration-fast) var(--ease); }
  /* Сессия занята делом: точка дышит, как активный пункт плана. Без движения — ровный тон. */
  .dot[data-live] { animation: card-pulse var(--duration-stage-pulse) var(--ease-stage-pulse) infinite; }
  @keyframes card-pulse { 50% { opacity: var(--opacity-stage-halo); } }
  .word[data-tone='blue'] { color: var(--blue); }
  .word[data-tone='success'] { color: var(--success); }
  .word[data-tone='warning'] { color: var(--warning); }
  .word[data-tone='danger'] { color: var(--danger); }
  .word[data-tone='muted'] { color: var(--muted); }
  .content { display: flex; flex-direction: column; gap: var(--s2); min-width: 0; padding: var(--s0) var(--s5) var(--s4) calc(var(--s4) + var(--icon-small) + var(--s3)); font-size: var(--fs-sm); }
  .fold { display: grid; grid-template-rows: 1fr; }
  .body { display: flex; flex-direction: column; gap: var(--s2); min-height: 0; overflow: hidden; padding-inline: var(--s2); }
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
