<script lang="ts">
  import type { InterruptControl } from '#lib/composer/interrupt.svelte.js';
  import Icon from '#lib/ui/Icon.svelte';
  import { liveLineText } from '#lib/timeline/present.js';
  import { leaseWaitText, type LeaseWait } from '#lib/format/lease.js';
  let {
    phase = 'working',
    tool = '',
    startedAt,
    now,
    interrupt = null,
    waiting = null
  }: { phase?: string; tool?: string; startedAt: number; now: number; interrupt?: InterruptControl | null; waiting?: LeaseWait | null } = $props();
  // Процесс стоит в очереди тяжёлых задач машины: место в очереди важнее имени инструмента.
  const text = $derived(waiting ? leaseWaitText(waiting) : liveLineText(phase, Math.max(0, now - startedAt), tool));
</script>

<div class="live" data-working data-lease-wait={waiting ? '' : undefined}>
  <span class="write" aria-hidden="true"><i></i><i></i><i></i></span><span class="text">{text}</span>
  <!-- Прервать ход можно здесь, вдали от «Отправить»: промах по кнопке отправки не прерывает работу. -->
  {#if interrupt}
    <button type="button" class="stop" data-interrupt aria-busy={interrupt.running} onclick={() => void interrupt.interrupt()}><Icon name="stop" /><span>{interrupt.label}</span></button>
  {/if}
</div>

<style>
  .live { display: flex; align-items: center; gap: var(--s3); margin: 0; color: var(--secondary); font-size: var(--fs-sm); font-variant-numeric: tabular-nums; }
  .text { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  /* Цель нажатия 44 px, а строка хода остаётся своей высоты: лишнее уходит в отрицательные поля. */
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

  /* Три слова одной строки, разной длины. Без движения они стоят написанными. */
  .write {
    flex: none;
    display: flex;
    align-items: center;
    gap: var(--work-write-gap);
    width: var(--work-write-width);
    height: var(--compact-fold-height);
    color: var(--accent);
  }
  .write i {
    height: var(--compact-fold-line);
    border-radius: var(--radius-pill);
    background: currentColor;
    transform-origin: left center;
  }
  .write i:nth-child(1) { flex: 5; animation: work-write-first var(--duration-work-write) var(--ease-work-write) infinite; }
  .write i:nth-child(2) { flex: 3; animation: work-write-second var(--duration-work-write) var(--ease-work-write) infinite; }
  .write i:nth-child(3) { flex: 4; animation: work-write-third var(--duration-work-write) var(--ease-work-write) infinite; }

  /* Слово выписывается слева направо, строка держится и стирается целиком: ритм письма, а не мигание. */
  @keyframes work-write-first {
    0% { transform: scaleX(0); opacity: 1; }
    14%, 76% { transform: scaleX(1); opacity: 1; }
    92%, 100% { transform: scaleX(1); opacity: 0; }
  }
  @keyframes work-write-second {
    0%, 18% { transform: scaleX(0); opacity: 1; }
    32%, 76% { transform: scaleX(1); opacity: 1; }
    92%, 100% { transform: scaleX(1); opacity: 0; }
  }
  @keyframes work-write-third {
    0%, 36% { transform: scaleX(0); opacity: 1; }
    50%, 76% { transform: scaleX(1); opacity: 1; }
    92%, 100% { transform: scaleX(1); opacity: 0; }
  }
  @media (prefers-reduced-motion: reduce) {
    .write i { animation: none; }
  }
</style>
