<script lang="ts">
  import { liveLineText } from '#lib/timeline/present.js';
  let { phase = 'working', tool = '', startedAt, now }: { phase?: string; tool?: string; startedAt: number; now: number } = $props();
  const text = $derived(liveLineText(phase, Math.max(0, now - startedAt), tool));
</script>

<p class="live" data-working><span class="write" aria-hidden="true"><i></i><i></i><i></i></span><span class="text">{text}</span></p>

<style>
  .live { display: flex; align-items: center; gap: var(--s3); margin: 0; color: var(--secondary); font-size: var(--fs-sm); font-variant-numeric: tabular-nums; }
  .text { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }

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
