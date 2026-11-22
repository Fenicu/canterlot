<script lang="ts">
  import { page } from '$app/state';
  import { announce } from '#lib/a11y/announce.js';
  import { fmt } from '#lib/format/ru.js';
  import type { ConnectionPhase } from '#lib/sync/connection.js';
  import type { ScopeStatus } from '#lib/sync/scope.js';
  import { arrive } from '#lib/ui/arrive.js';
  import Button from '#lib/ui/Button.svelte';
  import Icon from '#lib/ui/Icon.svelte';
  import InkIcon from '#lib/ui/InkIcon.svelte';
  import { swapOn } from '#lib/ui/swap.js';

  let {
    phase,
    since,
    now,
    scope,
    stale = false,
    lastAt = 0,
    ownSlot = false,
    update = 'idle',
    updateMessage = '',
    failure = '',
    onupdate,
    onrefresh
  }: {
    phase: ConnectionPhase;
    since: number;
    now: number;
    scope: ScopeStatus;
    /** Снимок старше пяти секунд: подпись говорит, на какое время он снят. */
    stale?: boolean;
    lastAt?: number;
    /** Своя полоса в потоке: список отдаёт место баннеру, а не прячет под ним строки. */
    ownSlot?: boolean;
    update?: string;
    updateMessage?: string;
    failure?: string;
    onupdate?: () => void;
    onrefresh: () => void;
  } = $props();

  type Tone = 'warning' | 'danger' | 'neutral' | 'success';
  interface View { kind: string; text: string; tone: Tone; action?: { label: string; run: () => void } }

  let previous = $state<ConnectionPhase | ''>('');
  let until = $state(0);
  let primed = false;

  $effect(() => {
    if (!primed) {
      primed = true;
      previous = phase;
      return;
    }
    if (phase === 'live' && previous === 'reconnecting') until = now + 2000;
    previous = phase;
  });

  // Баннер всегда один: при нескольких состояниях показывается самое важное.
  const view = $derived.by((): View | null => {
    if (update === 'failed') return { kind: 'update', text: updateMessage || 'Не удалось обновить приложение', tone: 'danger', action: { label: 'Повторить', run: () => onupdate?.() } };
    if (update !== 'idle') {
      return update === 'applying'
        ? { kind: 'update', text: 'Доступна новая версия. Сохраняю и обновляю…', tone: 'neutral' }
        : { kind: 'update', text: 'Доступна новая версия', tone: 'neutral', action: { label: 'Обновить', run: () => onupdate?.() } };
    }
    if (phase === 'incompatible') return { kind: 'incompatible', text: 'Доступна новая версия приложения', tone: 'neutral' };
    if (scope === 'degraded') return { kind: 'degraded', text: 'Не удаётся синхронизироваться. Повторяю попытки', tone: 'danger', action: { label: 'Обновить', run: onrefresh } };
    if (phase === 'offline') return { kind: 'link', text: 'Нет связи с сервером. Переподключаюсь…', tone: 'warning' };
    if (phase === 'reconnecting' && now - since >= 3000) return { kind: 'link', text: 'Нет связи. Переподключаюсь…', tone: 'warning' };
    if (failure) return { kind: 'failure', text: failure, tone: 'danger' };
    if (phase === 'live' && now < until) return { kind: 'link', text: 'Связь восстановлена', tone: 'success' };
    return null;
  });
  // Уходящая карточка дорисовывает последнее состояние, пока гаснет.
  let last: View | null = null;
  const shown = $derived.by(() => {
    if (view) last = view;
    return view ?? last;
  });
  const message = $derived(view && view.kind !== 'update' ? view.text : '');

  $effect(() => {
    if (message) announce(message);
  });

  // Слой лежит поверх экрана и встаёт под его верхнюю панель: шапка чата, строка поиска и вкладки списка
  // помечены data-top-chrome. Ничего не сдвигается, а кнопки «назад» и меню остаются открыты.
  let layer = $state<HTMLDivElement | null>(null);
  let offset = $state<number | null>(null);

  function place(node: HTMLDivElement) {
    const region = node.parentElement;
    if (!region) return;
    const origin = region.getBoundingClientRect().top;
    let bottom = -1;
    for (const chrome of region.querySelectorAll<HTMLElement>('[data-top-chrome]')) {
      const box = chrome.getBoundingClientRect();
      if (box.height > 0) bottom = Math.max(bottom, box.bottom - origin);
    }
    offset = bottom >= 0 ? Math.ceil(bottom) : null;
  }

  // Меряем до отрисовки: карточка не мелькнёт поверх шапки, которая уже есть в DOM.
  $effect.pre(() => {
    const node = layer;
    void page.url.pathname;
    if (!node || !view || ownSlot) return;
    place(node);
  });

  $effect(() => {
    const node = layer;
    void page.url.pathname;
    if (!node || !view || ownSlot) return;
    const region = node.parentElement!;
    let chrome = new Set<Element>();
    let frame = 0;
    let resize: ResizeObserver | null = null;

    const sync = () => {
      resize?.disconnect();
      resize = new ResizeObserver(() => place(node));
      resize.observe(region);
      chrome = new Set(region.querySelectorAll('[data-top-chrome]'));
      for (const item of chrome) resize.observe(item);
      place(node);
    };

    sync();

    // Шапка может смонтироваться позже баннера: открытие чата, reconnect или смена сессии.
    // Пересобираем набор верхних панелей одним кадром и замеряем заново.
    const mutations = new MutationObserver(() => {
      if (frame) return;
      frame = requestAnimationFrame(() => {
        frame = 0;
        const next = new Set(region.querySelectorAll('[data-top-chrome]'));
        if (next.size !== chrome.size || [...next].some((item) => !chrome.has(item))) sync();
      });
    });
    mutations.observe(region, { childList: true, subtree: true, attributes: true, attributeFilter: ['data-top-chrome'] });

    return () => {
      mutations.disconnect();
      resize?.disconnect();
      if (frame) cancelAnimationFrame(frame);
    };
  });

</script>

<div class="layer" class:ownSlot={ownSlot && !!view} bind:this={layer} style:--banner-top={offset === null ? undefined : `${offset}px`}>
  {#if view && shown}
    <div
      class="banner"
      data-tone={shown.tone}
      data-banner={shown.kind === 'update' || shown.kind === 'failure' ? undefined : shown.kind}
      data-update={shown.kind === 'update' ? '' : undefined}
      transition:arrive
    >
      <!-- У связи свой значок: пока её нет, это дышащий набросок; вернулась, и он прописывается один раз в тоне успеха. -->
      {#if shown.kind === 'link'}
        <span class="link" data-restored={shown.tone === 'success' ? '' : undefined}><InkIcon name={shown.tone === 'success' ? 'wifi' : 'wifiOff'} size="normal" state={shown.tone === 'success' ? 'ink' : 'sketch'} /></span>
      {:else}
        <Icon name={shown.tone === 'warning' || shown.tone === 'danger' ? 'warning' : 'info'} />
      {/if}
      <p><span class="words" use:swapOn={{ text: shown.text, key: `${shown.kind}:${shown.tone}` }}></span>{#if stale && lastAt > 0}<span class="stamp">· данные на {fmt.clock(lastAt)}</span>{/if}</p>
      {#if shown.action}<Button onclick={shown.action.run}>{shown.action.label}</Button>{/if}
    </div>
  {/if}
</div>

<style>
  /* Слой вне потока: он не занимает места, а касания мимо карточки проходят к экрану под ним. */
  .layer { position: absolute; z-index: 4; inset: var(--banner-top, calc(env(safe-area-inset-top) + var(--s4))) var(--s0) auto; display: flex; justify-content: center; padding-inline: max(var(--gutter), env(safe-area-inset-left)) max(var(--gutter), env(safe-area-inset-right)); pointer-events: none; }
  /* Список: баннеру отдана своя полоса, поэтому первая секция не прячется под ним. */
  .layer.ownSlot { position: static; inset: auto; z-index: auto; padding-block: var(--s2); }
  .banner { display: flex; align-items: center; gap: var(--s4); width: 100%; max-width: var(--reading-max); min-height: var(--target); padding: var(--s3) var(--s3) var(--s3) var(--s6); border-radius: var(--radius-card); background: var(--raised); color: var(--text); font-size: var(--fs-meta); pointer-events: auto; }
  [data-tone='warning'] { background: var(--warning-soft); }
  [data-tone='danger'] { background: var(--danger-soft); }
  .banner :global(svg) { flex: none; }
  /* Общий значок красится тоном баннера; значок связи красят чернила через --ink-*. */
  [data-tone='warning'] > :global(svg) { color: var(--warning); }
  [data-tone='danger'] > :global(svg) { color: var(--danger); }
  [data-tone='success'] > :global(svg) { color: var(--success); }
  .link { flex: none; display: inline-flex; --ink-live: var(--warning); --ink-rest: var(--warning); }
  .link[data-restored] { --ink-live: var(--success); --ink-rest: var(--success); }
  p { flex: 1; min-width: 0; margin: 0; padding-block: var(--s2); overflow-wrap: anywhere; }
  .words { display: inline-block; max-width: 100%; vertical-align: top; }
  .stamp { margin-inline-start: var(--s2); }
</style>
