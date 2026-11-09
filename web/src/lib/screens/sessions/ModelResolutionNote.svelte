<script lang="ts">
  import {
    changeSeen,
    markChangeSeen,
    resolutionChange,
    resolutionHint,
    type CatalogModelLike,
  } from '#lib/format/modelResolution.js';

  // `quiet`: форма уже назвала версию сессии, строка «псевдоним → модель» была бы повтором.
  let { entry, now = Date.now(), quiet = false }: { entry: CatalogModelLike | null | undefined; now?: number; quiet?: boolean } = $props();

  const hint = $derived(quiet ? '' : resolutionHint(entry, now));
  const change = $derived(resolutionChange(entry, now));
  // Уведомление о смене версии остаётся в форме, где его увидели, и в следующий раз уже не показывается.
  // Ключ намеренно не реактивный: отметка о прочтении не должна прятать уведомление сразу.
  let shown = '';
  const notice = $derived(change && (shown === change.key || !changeSeen(change.key)) ? change : null);
  $effect(() => {
    if (!notice) return;
    shown = notice.key;
    markChangeSeen(notice.key);
  });
</script>

{#if notice}<p class="notice" data-model-change>{notice.text}</p>{/if}
{#if hint}<p class="hint" data-model-resolution>{hint}</p>{/if}

<style>
  .hint { color: var(--muted); font-size: var(--fs-sm); margin: 0; overflow-wrap: anywhere; }
  .notice { color: var(--text); font-size: var(--fs-sm); margin: 0; }
</style>
