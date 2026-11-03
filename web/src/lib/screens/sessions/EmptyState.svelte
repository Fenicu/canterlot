<script lang="ts">
  import { resolve } from '$app/paths';
  let { kind, text = '' }: { kind: 'machines' | 'sessions' | 'search' | 'tab'; text?: string } = $props();
</script>

{#if kind === 'machines'}
  <p>Нет подключённых машин. Добавьте машину командой <code>canterlot machine add</code></p>
{:else if kind === 'sessions'}
  <p>Сессий пока нет.</p>
  <a href={resolve('new')}>Создать первую сессию</a>
{:else if kind === 'tab'}
  <p class="quiet">{text}</p>
{:else}
  <!-- Сбросить поиск можно кнопкой в самой строке поиска: вторая такая же здесь не нужна. -->
  <p>Ничего не найдено</p>
{/if}

<style>
  p, a { margin: var(--s4) var(--s6); }
  a { display: inline-flex; align-items: center; min-height: var(--target); color: var(--accent-link); }
  code { font-family: var(--font-mono); font-size: var(--fs-meta); overflow-wrap: anywhere; }
  .quiet { color: var(--muted); font-size: var(--fs-meta); text-align: center; }
</style>
