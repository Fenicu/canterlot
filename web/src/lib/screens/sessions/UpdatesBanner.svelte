<script lang="ts">
  import { goto } from '$app/navigation';
  import { resolve } from '$app/paths';
  import type { Runtime } from '#lib/app/runtime.js';
  import { updateText, updateTone } from '#lib/updates/text.js';
  import Button from '#lib/ui/Button.svelte';

  let { runtime }: { runtime: Runtime } = $props();

  const updates = $derived(runtime.updates);
  const unseen = $derived(updates.unseen);
  const shown = $derived(unseen.slice(0, 3));
  const extra = $derived(Math.max(0, unseen.length - shown.length));
  const tone = $derived(unseen.some((item) => updateTone(item.kind) === 'warning') ? 'warning' : 'neutral');

  function close() { updates.dismiss(); }
  function open() {
    // Адрес собран из resolve('/settings') и раздела: правило не видит resolve внутри шаблонной строки.
    // eslint-disable-next-line svelte/no-navigation-without-resolve
    void goto(`${resolve('/settings')}?c=updates`);
  }
</script>

{#if unseen.length > 0}
  <section class="banner" data-updates-banner data-tone={tone} aria-label="Новые обновления и модели">
    <div class="lines">
      {#each shown as item (item.id)}
        <p data-update-line data-tone={updateTone(item.kind)}>{updateText(item)}</p>
      {/each}
      {#if extra > 0}<p class="more" data-update-more>ещё {extra}</p>{/if}
    </div>
    <div class="actions">
      <Button onclick={open}>Открыть ленту</Button>
      <Button onclick={close}>Закрыть</Button>
    </div>
  </section>
{/if}

<style>
  .banner { display: flex; flex-direction: column; gap: var(--s3); margin: var(--s2) var(--s4); padding: var(--s4) var(--s5); border-radius: var(--radius-card); background: var(--surface); }
  .banner[data-tone='warning'] { background: var(--warning-soft); }
  .lines { display: flex; flex-direction: column; gap: var(--s1); min-width: 0; }
  .lines p { margin: var(--s0); color: var(--text); font-size: var(--fs-sm); overflow-wrap: anywhere; }
  .lines p[data-tone='warning'] { color: var(--warning); }
  .lines .more { color: var(--secondary); }
  .actions { display: flex; flex-wrap: wrap; gap: var(--s3); }
</style>
