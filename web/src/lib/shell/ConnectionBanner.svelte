<script lang="ts">
  import { announce } from '#lib/a11y/announce.js';
  import type { ConnectionPhase } from '#lib/sync/connection.js';
  import type { ScopeStatus } from '#lib/sync/scope.js';
  import Button from '#lib/ui/Button.svelte';
  import Icon from '#lib/ui/Icon.svelte';

  let {
    phase,
    since,
    now,
    scope,
    offline = '',
    update = 'idle',
    updateMessage = '',
    failure = '',
    onupdate,
    onrefresh,
    onrelogin
  }: {
    phase: ConnectionPhase;
    since: number;
    now: number;
    scope: ScopeStatus;
    offline?: string;
    update?: string;
    updateMessage?: string;
    failure?: string;
    onupdate?: () => void;
    onrefresh: () => void;
    onrelogin?: () => void;
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
    if (offline) return { kind: 'offline', text: offline, tone: 'warning' };
    if (update === 'failed') return { kind: 'update', text: updateMessage || 'Не удалось обновить приложение', tone: 'danger', action: { label: 'Повторить', run: () => onupdate?.() } };
    if (update !== 'idle') {
      return update === 'applying'
        ? { kind: 'update', text: 'Доступна новая версия. Сохраняю и обновляю…', tone: 'neutral' }
        : { kind: 'update', text: 'Доступна новая версия', tone: 'neutral', action: { label: 'Обновить', run: () => onupdate?.() } };
    }
    if (phase === 'incompatible') return { kind: 'incompatible', text: 'Доступна новая версия приложения', tone: 'neutral' };
    if (phase === 'auth_required') return { kind: 'auth', text: 'Сессия истекла. Войдите снова', tone: 'danger', action: { label: 'Войти', run: () => onrelogin?.() } };
    if (scope === 'degraded') return { kind: 'degraded', text: 'Не удаётся синхронизироваться. Повторяю попытки', tone: 'danger', action: { label: 'Обновить', run: onrefresh } };
    if (phase === 'offline') return { kind: 'link', text: 'Нет связи с сервером. Переподключаюсь…', tone: 'warning' };
    if (phase === 'reconnecting' && now - since >= 3000) return { kind: 'link', text: 'Нет связи. Переподключаюсь…', tone: 'warning' };
    if (failure) return { kind: 'failure', text: failure, tone: 'danger' };
    if (phase === 'live' && now < until) return { kind: 'link', text: 'Связь восстановлена', tone: 'success' };
    return null;
  });
  const message = $derived(view && view.kind !== 'offline' && view.kind !== 'update' ? view.text : '');

  $effect(() => {
    if (message) announce(message);
  });
</script>

{#if view}
  <div
    class="banner"
    data-tone={view.tone}
    data-banner={view.kind === 'offline' || view.kind === 'update' || view.kind === 'failure' ? undefined : view.kind}
    data-offline={view.kind === 'offline' ? '' : undefined}
    data-update={view.kind === 'update' ? '' : undefined}
  >
    <Icon name={view.tone === 'warning' || view.tone === 'danger' ? 'warning' : 'info'} />
    <p>{view.text}</p>
    {#if view.action}<Button onclick={view.action.run}>{view.action.label}</Button>{/if}
  </div>
{/if}

<style>
  .banner { display: flex; align-items: center; gap: var(--s4); margin: var(--s4) var(--gutter) var(--s0); padding: var(--s3) var(--s6); border-radius: var(--radius-card); background: var(--raised); color: var(--text); font-size: var(--fs-meta); }
  [data-tone='warning'] { background: var(--warning-soft); }
  [data-tone='danger'] { background: var(--danger-soft); }
  .banner :global(svg) { flex: none; }
  [data-tone='warning'] :global(svg) { color: var(--warning); }
  [data-tone='danger'] :global(svg) { color: var(--danger); }
  [data-tone='success'] :global(svg) { color: var(--success); }
  p { flex: 1; min-width: 0; margin: 0; overflow-wrap: anywhere; }
</style>
