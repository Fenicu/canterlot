<script lang="ts">
  import { prefersReducedMotion } from '#lib/motion.js';
  import { icons, type IconName } from './icons';
  import type { InkState } from './ink';

  /**
   * Значок «Чернила»: состояние вещи рисует сам значок. Состояние приходит свойством,
   * таймеров внутри нет: его выставляет тот, кто знает, что дело идёт, закончилось на глазах
   * или отклонено (`InkState`).
   *
   * Цвета: `--ink-rest` (стоит написанным, по умолчанию `--secondary`) и `--ink-live`
   * (идёт и пока пишется, по умолчанию `--accent-link`) задаёт хозяин значка. При выключенном
   * движении, системном или масштабом 0, нет ни дыхания, ни росчерка: значок сразу стоит в конечном виде.
   */
  let { name, state = 'dry', size = 'small' }: { name: IconName; state?: InkState; size?: 'small' | 'normal' } = $props();

  // Читается заново при каждой смене состояния: настройка движения могла измениться между делами.
  const motion = $derived.by(() => {
    void state;
    return prefersReducedMotion() ? 'off' : 'on';
  });
</script>

<svg class="mark" class:normal={size === 'normal'} data-state={state} data-motion={motion} viewBox="0 0 24 24" aria-hidden="true" focusable="false">
  <path class="ghost" d={icons[name]} />
  <path class="ink" d={icons[name]} pathLength="1" />
</svg>

<style>
  .mark { display: block; flex: none; width: var(--icon-small); height: var(--icon-small); fill: none; stroke: currentColor; stroke-width: var(--focus-width); color: var(--ink-rest, var(--secondary)); }
  .mark.normal { width: var(--icon); height: var(--icon); }

  /* Основа каждого состояния — его конечный вид: движение только добавляется поверх, и без него значок уже верный. */
  .ghost { opacity: 0; }

  /* Идёт: контур в тоне акцента, чернил нет. */
  .mark[data-state='sketch'] { color: var(--ink-live, var(--accent-link)); }
  .mark[data-state='sketch'] .ink { opacity: 0; }
  .mark[data-state='sketch'] .ghost { opacity: var(--opacity-dim); }

  /* Готово на глазах: контур остаётся под чернилами, цвет стоит в покое. Росчерк и оседание цвета ниже. */
  .mark[data-state='ink'] .ghost { opacity: var(--opacity-low); }
  .mark[data-state='ink'] .ink { stroke-dasharray: 1 1.1; }

  /* Отказ: ничего не записано, значок остаётся приглушённым наброском. */
  .mark[data-state='refused'] { color: var(--muted); }
  .mark[data-state='refused'] .ink { opacity: 0; }
  .mark[data-state='refused'] .ghost { opacity: var(--opacity-dim); }

  /* Дыхание: только прозрачность, кадр собирает композитор; идёт лишь пока дело идёт. */
  .mark[data-state='sketch'][data-motion='on'] .ghost { animation: mark-breathe var(--duration-stage-pulse) var(--ease-stage-pulse) infinite alternate; }
  @keyframes mark-breathe { from { opacity: var(--opacity-low); } to { opacity: var(--opacity-dim); } }

  /* Росчерк: чернила проходят по контуру один раз, потом цвет из тона акцента оседает в покой. */
  .mark[data-state='ink'][data-motion='on'] .ink { animation: mark-ink var(--duration-ink) var(--ease-ink) backwards; }
  .mark[data-state='ink'][data-motion='on'] { animation: mark-settle var(--duration-expand) var(--ease) var(--duration-ink) backwards; }
  @keyframes mark-ink { from { stroke-dashoffset: 1.05; } to { stroke-dashoffset: 0; } }
  @keyframes mark-settle { from { color: var(--ink-live, var(--accent-link)); } }
</style>
