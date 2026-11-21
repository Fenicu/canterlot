<script lang="ts">
  import { onMount } from 'svelte';
  import { arrive, leave } from '#lib/ui/arrive.js';

  let {
    onundo,
    onclose,
    timeout = 6000
  }: {
    onundo: () => void;
    onclose: () => void;
    timeout?: number;
  } = $props();

  onMount(() => {
    const timer = window.setTimeout(onclose, timeout);
    return () => window.clearTimeout(timer);
  });
</script>

<!-- Внешний слой расставляет тост по центру, внутренний приходит и уходит: переход пишет transform, а центровка не должна его терять.
     Переход глобальный, потому что тост убирает условный блок хозяина, а не свой. -->
<div class="layer">
  <div class="toast" role="status" aria-live="polite" in:arrive|global out:leave|global>
    <span>Сессия в корзине</span>
    <span class="dot" aria-hidden="true">·</span>
    <button type="button" onclick={onundo}>Вернуть</button>
  </div>
</div>

<style>
  .layer {
    position: fixed;
    z-index: 6;
    inset: auto var(--s0) calc(var(--target) + var(--s6));
    display: flex;
    justify-content: center;
    pointer-events: none;
  }
  .toast {
    display: flex;
    align-items: center;
    gap: var(--s3);
    max-width: 90vw;
    padding: var(--s3) var(--s5);
    border-radius: var(--radius-pill);
    background: var(--surface);
    color: var(--text);
    box-shadow: var(--shadow);
    font-size: var(--fs-sm);
    pointer-events: auto;
  }
  .dot { color: var(--muted); }
  button {
    min-height: var(--target);
    padding-inline: var(--s3);
    border: var(--s0);
    background: transparent;
    color: var(--accent-link);
    font-weight: var(--weight-medium);
  }
</style>
