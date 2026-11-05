<script lang="ts">
  import { onMount } from 'svelte';

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

<div class="toast" role="status" aria-live="polite">
  <span>Сессия в корзине</span>
  <span class="dot" aria-hidden="true">·</span>
  <button type="button" onclick={onundo}>Вернуть</button>
</div>

<style>
  .toast {
    position: fixed;
    z-index: 6;
    left: 50%;
    bottom: calc(var(--target) + var(--s6));
    transform: translateX(-50%);
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
