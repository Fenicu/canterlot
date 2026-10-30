<script lang="ts">
  import { resolve } from '$app/paths';
  let { kind, onreset }: { kind: 'machines' | 'sessions' | 'search'; onreset?: () => void } = $props();
</script>

{#if kind === 'machines'}
  <p>Нет подключённых машин. Добавьте машину командой <a href={resolve('/')}>machine add</a></p>
{:else if kind === 'sessions'}
  <p>Сессий пока нет.</p>
  <a href={resolve('new')}>Создать первую сессию</a>
{:else}
  <p>Ничего не найдено</p>
  <button type="button" onclick={() => onreset?.()}>Сбросить</button>
{/if}

<style>
  p, a, button { margin: var(--s4) var(--s6); }
  a, button { min-height: var(--target); color: var(--accent-link); background: var(--transparent); border: var(--s0); }
  p a { margin: var(--s0); }
</style>
