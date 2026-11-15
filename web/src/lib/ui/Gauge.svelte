<script lang="ts">
  import { styleProperty } from './style-property';
  let { value, warn = false, tone = 'normal', dim = false, quiet = false, label }: { value: number; warn?: boolean; tone?: 'normal' | 'warning' | 'danger'; dim?: boolean; /** Здоровый остаток — тихая заливка: внимание достаётся тесному окну. */ quiet?: boolean; label: string } = $props();
  const bounded = $derived(Math.min(100, Math.max(0, Number.isFinite(value) ? value : 0)));
  const level = $derived(tone === 'normal' && warn ? 'warning' : tone);
</script>
<div role="meter" aria-label={label} aria-valuemin="0" aria-valuemax="100" aria-valuenow={bounded} data-tone={level} data-quiet={quiet && level === 'normal' ? '' : undefined} class:warn={level === 'warning'} class:dim use:styleProperty={{ name: '--fill', value: `${bounded}%` }}><span></span></div>
<style>
  div { flex: none; width: var(--gauge-width); max-width: 100%; height: var(--s3); overflow: hidden; background: var(--line); border-radius: var(--radius-pill); align-self: center; }
  /* Заливка во всю дорожку и сдвинута влево на непрожитую долю: новое показание доезжает переносом, без пересчёта ширины.
     Край дорожки обрезает заливку своей формой: своё скругление у заливки сплющивало узкий остаток в овал. */
  span { display: block; height: 100%; width: 100%; background: var(--accent); transform: translateX(calc(var(--fill) - 100%)); transition: transform var(--duration-gauge) var(--ease-gauge); }
  [data-tone='warning'] span { background: var(--warning); }
  [data-tone='danger'] span { background: var(--danger); }
  /* Здоровая заливка не спорит с тревожной: тон ведёт тревога, а не запас. */
  [data-quiet] span { background: var(--muted); }
  .dim span { opacity: var(--opacity-dim); }
</style>
