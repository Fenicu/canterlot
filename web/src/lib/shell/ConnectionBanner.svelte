<script lang="ts">
  import { announce } from '$lib/a11y/announce';
  import type { ConnectionPhase } from '$lib/sync/connection';
  import type { ScopeStatus } from '$lib/sync/scope';

  let {
    phase,
    since,
    now,
    scope,
    onrefresh
  }: {
    phase: ConnectionPhase;
    since: number;
    now: number;
    scope: ScopeStatus;
    onrefresh: () => void;
  } = $props();

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

  const message = $derived.by(() => {
    if (phase === 'incompatible') return 'Доступна новая версия приложения';
    if (scope === 'degraded') return 'Не удаётся синхронизироваться. Повтор через 30 с';
    if (phase === 'reconnecting' && now - since >= 3000) return 'Нет связи. Переподключаюсь…';
    if (phase === 'live' && now < until) return 'Связь восстановлена';
    return '';
  });

  $effect(() => {
    if (message) announce(message);
  });
</script>

{#if message}
  <p class="banner" data-banner={phase === 'incompatible' ? 'incompatible' : scope === 'degraded' ? 'degraded' : 'link'}>
    {message}
    {#if scope === 'degraded'}
      <button type="button" onclick={onrefresh}>Обновить</button>
    {/if}
  </p>
{/if}

<style>
  .banner {
    margin: var(--s4) var(--s6) 0;
    padding: var(--s4) var(--s6);
    border-radius: var(--radius-pill);
    background: var(--raised);
    color: var(--text);
  }
  button { min-height: var(--target); margin-left: var(--s4); border: var(--s0); background: var(--transparent); color: var(--accent-link); }
</style>
