<script lang="ts">
  import { onDestroy } from 'svelte';
  import { COPY_DONE, COPY_FAILED } from '#lib/render/copy.js';
  import Icon from '#lib/ui/Icon.svelte';

  /** Кнопка копирует `text` как есть: исходную команду, а не разложенную для показа. */
  let { text, label }: { text: string; label: string } = $props();
  let status = $state('');
  let timer: ReturnType<typeof setTimeout> | null = null;

  async function copy(): Promise<void> {
    try {
      if (typeof navigator === 'undefined' || !navigator.clipboard?.writeText) throw new Error('clipboard_unavailable');
      await navigator.clipboard.writeText(text);
      status = COPY_DONE;
    } catch {
      status = COPY_FAILED;
    }
    if (timer) clearTimeout(timer);
    timer = setTimeout(() => {
      status = '';
      timer = null;
    }, 2000);
  }

  onDestroy(() => {
    if (timer) clearTimeout(timer);
  });
</script>

<span class="copy">
  <span class="status" role="status" data-failed={status === COPY_FAILED ? '' : undefined}>{status}</span>
  <button type="button" aria-label={label} data-role="copy" onclick={copy}><Icon name="copy" /></button>
</span>

<style>
  .copy { display: inline-flex; flex: none; align-items: center; gap: var(--s2); margin-inline-start: auto; }
  .status { color: var(--secondary); font-size: var(--fs-sm); }
  .status:empty { display: none; }
  .status[data-failed] { color: var(--danger); }
  button { display: inline-flex; align-items: center; justify-content: center; min-width: var(--target); min-height: var(--target); padding: var(--s0); border-radius: var(--radius-round); background: var(--transparent); color: var(--secondary); }
  button :global(svg) { width: var(--icon-small); height: var(--icon-small); }
  button:active { transform: scale(var(--scale-press)); }
</style>
