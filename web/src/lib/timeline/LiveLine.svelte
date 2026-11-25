<script lang="ts">
  import type { InterruptControl } from '#lib/composer/interrupt.svelte.js';
  import Icon from '#lib/ui/Icon.svelte';
  import WorkMotif from '#lib/ui/WorkMotif.svelte';
  import { swapOn } from '#lib/ui/swap.js';
  import { liveLineText } from '#lib/timeline/present.js';
  import { leaseWaitText, type LeaseWait } from '#lib/format/lease.js';
  let {
    phase = 'working',
    tool = '',
    startedAt,
    now,
    interrupt = null,
    waiting = null,
    quiet = false
  }: { phase?: string; tool?: string; startedAt: number; now: number; interrupt?: InterruptControl | null; waiting?: LeaseWait | null; quiet?: boolean } = $props();
  // Процесс стоит в очереди тяжёлых задач машины: место в очереди важнее имени инструмента.
  const text = $derived(waiting ? leaseWaitText(waiting) : liveLineText(phase, Math.max(0, now - startedAt), tool));
  // Занятие («Генерирует», «Выполняет Bash») сменяется подписью, а часы хода после « · » тикают на месте без движения.
  // Разделитель живёт в подписи: подпись на миг уходит при смене фазы, и «· 12 с» без слова не появляется.
  const parts = $derived.by(() => {
    const cut = waiting ? -1 : text.lastIndexOf(' · ');
    return cut < 0 ? { label: text, tail: '' } : { label: text.slice(0, cut), tail: ` ${text.slice(cut + 3)}` };
  });
  const labelText = $derived(parts.tail ? `${parts.label} ·` : parts.label);
</script>

<div class="live" data-working data-quiet={quiet ? '' : undefined} data-lease-wait={waiting ? '' : undefined}>
  <!-- В очереди тяжёлых задач ничего ещё не пишется: перо уступает стоящий молоток. -->
  {#if waiting}<span class="queued" aria-hidden="true"><Icon name="hammer" /></span>{:else}<WorkMotif />{/if}<span class="text" class:sr-only={quiet}><span class="label" use:swapOn={{ text: labelText, key: parts.label }}></span><span class="tail">{parts.tail}</span></span>
  <!-- Прервать ход можно здесь, вдали от «Отправить»: промах по кнопке отправки не прерывает работу. -->
  {#if interrupt}
    <button type="button" class="stop" data-interrupt aria-busy={interrupt.running} onclick={() => void interrupt.interrupt()}><Icon name="stop" /><span>{interrupt.label}</span></button>
  {/if}
</div>

<style>
  /* Строка хода остаётся своей высоты, а цель «Прервать» выступает над ней и под ней на половину
     остатка цели. Недостающие до соседей зазоры отдаём строке: сверху до строки инструмента,
     снизу до подвала карточки, иначе цель ложится на их кнопки. */
  .live { display: flex; align-items: center; gap: var(--s3); margin-block: calc((var(--target) - var(--icon)) / 2 - var(--s2)) calc((var(--target) - var(--icon)) / 2 - var(--s2) * 2); color: var(--secondary); font-size: var(--fs-sm); font-variant-numeric: tabular-nums; }
  .text { display: flex; min-width: 0; white-space: nowrap; }
  .label { flex: 0 1 auto; min-width: 0; overflow: hidden; text-overflow: ellipsis; }
  .tail { flex: none; white-space: pre; }
  .queued { flex: none; display: inline-flex; color: var(--secondary); }
  .queued :global(svg) { width: var(--icon-small); height: var(--icon-small); }
  /* Цель нажатия 44 px держится на строке, а лишнее уходит в отрицательные поля. */
  .stop {
    flex: none;
    display: inline-flex;
    align-items: center;
    gap: var(--s3);
    min-height: var(--target);
    margin-block: var(--icon-hang);
    margin-inline-start: auto;
    padding-inline: var(--s6);
    border: var(--s0);
    border-radius: var(--radius-pill);
    background: var(--transparent);
    color: var(--secondary);
    font-size: var(--fs-sm);
    font-weight: var(--weight-medium);
    white-space: nowrap;
  }
  .stop:hover { background: var(--hover); color: var(--text); }
  .stop :global(svg) { width: var(--icon-small); height: var(--icon-small); }
</style>
