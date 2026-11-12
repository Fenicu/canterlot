<script lang="ts">
  import { page } from '$app/state';
  import { announce } from '#lib/a11y/announce.js';
  import { durationMs, easing, fadeMs, prefersReducedMotion } from '#lib/motion.js';
  import type { ConnectionPhase } from '#lib/sync/connection.js';
  import type { ScopeStatus } from '#lib/sync/scope.js';
  import Button from '#lib/ui/Button.svelte';
  import Icon from '#lib/ui/Icon.svelte';

  let {
    phase,
    since,
    now,
    scope,
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

  $effect(() => {
    const node = layer;
    void page.url.pathname;
    if (!node || !view) return;
    place(node);
    const region = node.parentElement!;
    const observer = new ResizeObserver(() => place(node));
    observer.observe(region);
    for (const chrome of region.querySelectorAll('[data-top-chrome]')) observer.observe(chrome);
    return () => observer.disconnect();
  });

  /** Появление: короткий сдвиг на токен --shift-enter и прозрачность. Без движения остаётся только прозрачность. */
  function arrive(node: HTMLElement) {
    if (prefersReducedMotion()) return { duration: fadeMs('--duration-fast'), css: (t: number) => `opacity: ${t}` };
    const shift = parseFloat(getComputedStyle(node).getPropertyValue('--shift-enter')) || 0;
    return {
      duration: durationMs('--duration-enter'),
      easing: easing('--ease-enter'),
      css: (t: number) => `opacity: ${t}; transform: translateY(${(t - 1) * shift}px)`
    };
  }
</script>

<div class="layer" bind:this={layer} style:--banner-top={offset === null ? undefined : `${offset}px`}>
  {#if view && shown}
    <div
      class="banner"
      data-tone={shown.tone}
      data-banner={shown.kind === 'update' || shown.kind === 'failure' ? undefined : shown.kind}
      data-update={shown.kind === 'update' ? '' : undefined}
      transition:arrive
    >
      <Icon name={shown.tone === 'warning' || shown.tone === 'danger' ? 'warning' : 'info'} />
      <p>{shown.text}</p>
      {#if shown.action}<Button onclick={shown.action.run}>{shown.action.label}</Button>{/if}
    </div>
  {/if}
</div>

<style>
  /* Слой вне потока: он не занимает места, а касания мимо карточки проходят к экрану под ним. */
  .layer { position: absolute; z-index: 4; inset: var(--banner-top, calc(env(safe-area-inset-top) + var(--s4))) var(--s0) auto; display: flex; justify-content: center; padding-inline: max(var(--gutter), env(safe-area-inset-left)) max(var(--gutter), env(safe-area-inset-right)); pointer-events: none; }
  .banner { display: flex; align-items: center; gap: var(--s4); width: 100%; max-width: var(--reading-max); min-height: var(--target); padding: var(--s3) var(--s3) var(--s3) var(--s6); border-radius: var(--radius-card); background: var(--raised); color: var(--text); font-size: var(--fs-meta); pointer-events: auto; }
  [data-tone='warning'] { background: var(--warning-soft); }
  [data-tone='danger'] { background: var(--danger-soft); }
  .banner :global(svg) { flex: none; }
  [data-tone='warning'] :global(svg) { color: var(--warning); }
  [data-tone='danger'] :global(svg) { color: var(--danger); }
  [data-tone='success'] :global(svg) { color: var(--success); }
  p { flex: 1; min-width: 0; margin: 0; padding-block: var(--s2); overflow-wrap: anywhere; }
</style>
