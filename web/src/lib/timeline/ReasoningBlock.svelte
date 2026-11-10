<script lang="ts">
  import { untrack } from 'svelte';
  import { fmt } from '#lib/format/ru.js';
  import { scaledDurationMs } from '#lib/motion.js';
  import { reasoningPref } from '#lib/screens/settings/preferences.js';
  import type { BlockModel } from '#lib/stores/blocks.js';
  import type { FeedFolds } from '#lib/timeline/folds.js';
  import { liveElapsed } from '#lib/timeline/present.js';
  import {
    reasoningDuration,
    reasoningPresentation,
    reasoningPreview,
    reasoningTail,
    type ReasoningChoice,
    type ReasoningPresentation
  } from '#lib/timeline/reasoning.js';
  import TextBlock from '#lib/timeline/TextBlock.svelte';
  import Icon from '#lib/ui/Icon.svelte';
  import { blockFold } from '#lib/ui/fold.js';

  let { block, folds, now }: { block: BlockModel; folds: FeedFolds; now: number } = $props();

  /** Сколько живое окно стоит после финала: строке дают дочитаться, а потом она складывается. */
  const LIVE_PAUSE_MS = 600;

  const streaming = $derived(block.state === 'streaming' || block.state === 'stale');
  const choice = $derived.by((): ReasoningChoice => {
    const value = folds.choice(block.id);
    return value === undefined ? null : value ? 'open' : 'closed';
  });
  const preference = $derived(reasoningPref());
  const base = $derived(reasoningPresentation(preference, streaming, choice));

  // Живое окно уже свёрнуто: блок, пришедший готовым, не раскрывается задним числом.
  let settled = $state(untrack(() => !streaming));
  let wasStreaming = untrack(() => streaming);

  $effect(() => {
    if (streaming) {
      wasStreaming = true;
      settled = false;
      return;
    }
    if (!wasStreaming) {
      if (preference !== 'live' || choice !== null) settled = true;
      return;
    }
    wasStreaming = false;
    // Окно остаётся только там, где его открыла настройка: выбор владельца и «Строкой» не ждут.
    if (preference !== 'live' || choice !== null) {
      settled = true;
      return;
    }
    const pause = scaledDurationMs(LIVE_PAUSE_MS);
    if (pause <= 0) {
      settled = true;
      return;
    }
    const timer = setTimeout(() => {
      // Пока шла пауза, владелец мог раскрыть блок или сменить настройку: тогда окно не трогаем.
      if (reasoningPref() !== 'live' || folds.choice(block.id) !== undefined) return;
      // Сворачивание идёт тем же путём, что и ручное: лента у хвоста остаётся прижатой.
      folds.autoChange(true);
      settled = true;
    }, pause);
    return () => clearTimeout(timer);
  });

  const liveWindow = $derived(preference === 'live' && choice === null && !settled);
  const shown = $derived<ReasoningPresentation>(liveWindow && base === 'collapsed' ? 'live' : base);
  const tail = $derived(reasoningTail(block.text));
  const preview = $derived(
    shown === 'collapsed' ? reasoningPreview(block.text, streaming ? 'live' : 'first') : ''
  );
  const running = $derived(fmt.duration(liveElapsed(now - (block.startedAt ?? block.createdAt))));
  const duration = $derived(streaming ? running : reasoningDuration(block));

  function toggle(): void {
    // Тап по живому окну раскрывает блок целиком, а не сворачивает его: окно уже открыто.
    folds.toggle(block.id, shown === 'expanded');
  }
</script>

<div class="reasoning" data-open={shown !== 'collapsed' ? '' : undefined} data-presentation={shown}>
  <button type="button" class="toggle" aria-expanded={shown !== 'collapsed'} onclick={toggle}>
    <span class="chevron"><Icon name="chevron" /></span>
    <span class="label">{streaming ? 'Рассуждает' : 'Рассуждение'}</span>
    {#if preview}
      <span class="sep" aria-hidden="true">·</span>
      <span class="preview">{preview}</span>
    {/if}
    {#if duration && shown === 'live'}
      <span class="sep" aria-hidden="true">·</span>
      <span class="duration live">{duration}</span>
    {:else if duration}
      <span class="duration">{duration}</span>
    {/if}
  </button>
  {#if shown !== 'collapsed'}
    <div class="fold" transition:blockFold>
      {#if shown === 'live'}
        <!-- Поток идёт дешёвым текстом: Markdown здесь переразбирался бы на каждую дельту. -->
        <div class="window" data-role="reasoning-window" aria-live="off"><div class="tail">{tail}</div></div>
      {:else}
        <div class="body"><TextBlock {block} /></div>
      {/if}
    </div>
  {/if}
</div>

<style>
  .reasoning { display: flex; flex-direction: column; min-width: 0; margin-block: calc(var(--s2) * -1); color: var(--secondary); }
  .toggle { display: flex; align-items: center; gap: var(--s3); width: 100%; min-width: 0; min-height: var(--target); padding: var(--s0) var(--s4); border-radius: var(--radius-pill); background: var(--transparent); color: var(--secondary); font-size: var(--fs-sm); text-align: start; }
  .chevron { flex: none; color: var(--muted); transition: transform var(--duration-expand) var(--ease-expand); }
  .chevron :global(svg) { width: var(--icon-small); height: var(--icon-small); }
  [data-open] .chevron { transform: rotate(90deg); }
  .label { flex: none; white-space: nowrap; }
  .sep { flex: none; color: var(--muted); }
  /* Длинная строка не расширяет карточку: хвост обрезается многоточием по ширине строки. */
  .preview { flex: 1 1 auto; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .duration { flex: none; margin-inline-start: auto; color: var(--muted); font-variant-numeric: tabular-nums; white-space: nowrap; }
  /* В живом окне часы стоят рядом со словом, как в строке хода: «Рассуждает · 12 с». */
  .duration.live { margin-inline-start: var(--s0); }
  .fold { display: grid; grid-template-rows: 1fr; }
  /* clip, а не hidden: иначе тело прокручиваемо, и ссылка внутри уезжает из-под пальца посреди раскрытия. */
  .body { min-height: 0; overflow: clip; font-size: var(--fs-meta); }
  /* Окно держит шесть строк на любой длине потока: высота не меняется от дельты к дельте. */
  .window {
    position: relative;
    height: calc(var(--fs-meta) * var(--lh-body) * 6);
    overflow: clip;
    color: var(--secondary);
    font-size: var(--fs-meta);
    line-height: var(--lh-body);
    -webkit-mask-image: linear-gradient(to bottom, transparent, currentColor var(--s10));
    mask-image: linear-gradient(to bottom, transparent, currentColor var(--s10));
  }
  /* Хвост прижат к низу: новые строки появляются внизу, старые уезжают вверх под градиент. */
  .tail { position: absolute; inset: auto var(--s4) 0; white-space: pre-wrap; overflow-wrap: anywhere; }
</style>
