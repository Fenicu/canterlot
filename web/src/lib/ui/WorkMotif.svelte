<!-- Мотив работы: три строки выписываются слева направо. Общий для живой строки хода и карточек, которые что-то собирают. -->
<span class="write" aria-hidden="true"><i></i><i></i><i></i></span>

<style>
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
  /* Снимок без связи: движение пера и акцент гаснут, текст остаётся на месте. */
  :global([data-stale]) .write { color: var(--muted); }
  :global([data-stale]) .write i { animation: none; }
</style>
