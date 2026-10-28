<script lang="ts">
  import { styleProperty } from './style-property';
  import { onMount } from 'svelte';
  import { announce } from '../a11y/announce';
  import { prefersReducedMotion } from '../motion';
  import { swapLabel, type Stage, type StageState } from './stage-track';
  let { stages, index, state: trackState = 'active', form = 'compact', label, status, reason, progress, countdown }: {
    stages: Stage[]; index: number; state?: StageState; form?: 'compact' | 'named'; label: string;
    status?: string; reason?: string; progress?: number; countdown?: { from: number; durationMs: number };
  } = $props();
  let host: HTMLDivElement;
  let instant = $state(true);
  let mounted = $state(false);
  let landed = $state(false);
  let initial = true;
  let box = $state(0);
  let ringInset = $state(0);
  const current = $derived(Math.min(stages.length - 1, Math.max(-1, index)));
  const phase = $derived(trackState === 'active' && current === stages.length - 1 ? 'done' : trackState);
  const summary = $derived(trackState === 'error' ? reason ?? 'Ошибка' : status ?? stages[current]?.label ?? 'Ожидание');
  const fill = $derived(stages.length < 2 ? 0 : Math.max(0, trackState === 'error' ? current - 1 : current) / (stages.length - 1));
  const fraction = $derived(Math.min(1, Math.max(0, progress ?? countdown?.from ?? 0)));
  let previous = '';
  $effect(() => {
    if (!mounted) return;
    const next = `${current}:${phase}`;
    if (previous === '') {
      previous = next;
      return;
    }
    if (next !== previous) {
      previous = next;
      announce(`${label}: ${summary}`);
    }
  });
  $effect(() => {
    if (!mounted) return;
    const isDone = phase === 'done';
    if (initial) {
      initial = false;
      return;
    }
    if (isDone && !landed) {
      landed = true;
    } else if (!isDone && landed) {
      landed = false;
    }
  });
  $effect(() => {
    if (!mounted) return;
    const style = getComputedStyle(host);
    const token = style.getPropertyValue(form === 'compact' ? '--icon-small' : '--s10');
    box = parseFloat(token);
    ringInset = parseFloat(style.getPropertyValue('--stage-ring-inset'));
  });
  $effect(() => {
    if (!mounted || !countdown || prefersReducedMotion()) return;
    const arc = host.querySelector<SVGCircleElement>('[aria-current="step"] .st-ring-arc');
    if (!arc) return;
    const animation = arc.animate([{ strokeDasharray: `${countdown.from * 100} 100` }, { strokeDasharray: '100 100' }], { duration: countdown.durationMs, easing: 'linear', fill: 'forwards' });
    const media = matchMedia('(prefers-reduced-motion: reduce)');
    const stop = () => { if (prefersReducedMotion()) animation.cancel(); };
    media.addEventListener('change', stop);
    const observer = new MutationObserver(stop);
    observer.observe(document.documentElement, { attributes: true, attributeFilter: ['style', 'data-motion'] });
    return () => { animation.cancel(); observer.disconnect(); media.removeEventListener('change', stop); };
  });
  onMount(() => {
    mounted = true;
    let second = 0;
    const first = requestAnimationFrame(() => { second = requestAnimationFrame(() => { instant = false; }); });
    return () => { cancelAnimationFrame(first); cancelAnimationFrame(second); };
  });
</script>
<div bind:this={host} class="st" class:st--compact={form === 'compact'} class:st--labeled={form === 'named'} role="group" aria-label={label} data-instant={instant ? '' : undefined} data-state={phase} use:styleProperty={{ name: '--st-count', value: String(stages.length) }}>
  <div class="st-track"><span class="st-line" aria-hidden="true"><span class="st-fill" use:styleProperty={{ name: 'transform', value: `scaleX(${fill})` }}></span></span>
    <ol class="st-points">
      {#each stages as stage, k (stage.id)}
        <li class="st-point" data-state={k < current ? 'done' : k === current ? phase : 'pending'} data-kind={stage.kind ?? 'pulse'} data-landed={landed && k === current ? '' : undefined} aria-current={k === current ? 'step' : undefined}>
          <span class="st-mark" aria-hidden="true"><span class="st-halo"></span>
            <svg class="st-ring" viewBox={`0 0 ${box} ${box}`}>
              <circle class="st-ring-track" cx={box / 2} cy={box / 2} r={box / 2 - ringInset} pathLength="100" transform={`rotate(-90 ${box / 2} ${box / 2})`} />
              <circle class="st-ring-arc" cx={box / 2} cy={box / 2} r={box / 2 - ringInset} pathLength="100" transform={`rotate(-90 ${box / 2} ${box / 2})`} use:styleProperty={{ name: 'transition', value: countdown ? 'none' : undefined }} use:styleProperty={{ name: 'stroke-dasharray', value: stage.kind === 'ring' && k === current ? `${fraction * 100} 100` : undefined }} />
            </svg><span class="st-dot"></span>
          </span><span class="st-label" use:swapLabel={stage.label}></span>
        </li>
      {/each}
    </ol>
  </div>
  {#if form === 'compact'}<span class="st-status" data-tone={phase === 'error' ? 'error' : phase === 'held' ? 'quiet' : phase === 'done' ? 'done' : 'normal'} use:swapLabel={summary}></span>{:else if phase === 'error'}<p class="st-error">{reason ?? 'Ошибка'}</p>{/if}
</div>
<style>

.st {
  --st-on: var(--accent);
  --st-error: var(--danger);
  --st-base: var(--line);
  --st-pending: var(--badge);
  --st-surface: var(--surface);
  --st-box: var(--stage-box);
  --st-dot: var(--stage-dot);
  --st-line: var(--stage-line);
  --st-fill-duration: var(--duration-stage-fill-compact);
  --st-arrive-delay: calc(var(--st-fill-duration) * var(--stage-arrive-at));
  --st-halo-delay: calc(var(--st-arrive-delay) + var(--duration-stage-arrive));
  position: relative;
  min-width: var(--s0);
}
.st-track { position: relative; }
.st-points { position: relative; list-style: none; margin: var(--s0); padding: var(--s0); }
.st-line {
  position: absolute;
  height: var(--st-line);
  top: calc((var(--st-box) - var(--st-line)) / 2);
  border-radius: var(--radius-pill);
  background: var(--st-base);
  overflow: hidden;
}
.st-fill {
  display: block;
  width: 100%;
  height: 100%;
  background: var(--st-on);
  transform-origin: left center;
  transform: scaleX(0);
  transition: transform var(--st-fill-duration) var(--ease-stage-fill);
}
.st-point { position: relative; display: flex; align-items: center; min-width: var(--s0); }
.st-mark { position: relative; display: grid; place-items: center; width: var(--st-box); height: var(--st-box); flex-shrink: 0; }
.st-mark > * { grid-area: 1 / 1; }

.st-dot {
  width: var(--st-dot);
  height: var(--st-dot);
  border-radius: var(--radius-round);
  background: var(--st-surface);
  box-shadow: inset 0 0 0 var(--stage-dot-ring) var(--st-pending);
  transition: background-color var(--duration-fast) var(--ease) var(--st-arrive-delay), box-shadow var(--duration-fast) var(--ease) var(--st-arrive-delay);
}
.st-halo { width: var(--st-dot); height: var(--st-dot); border-radius: var(--radius-round); background: var(--st-on); opacity: 0; pointer-events: none; }
.st-ring { width: var(--st-box); height: var(--st-box); overflow: visible; fill: none; opacity: 0; }
.st-ring circle { stroke-width: var(--stage-dot-ring); }
.st-ring-track { stroke: var(--st-base); fill: var(--st-surface); }
.st-ring-arc { stroke: var(--st-on); stroke-dasharray: 0 100; transition: stroke-dasharray var(--duration-stage-progress) var(--ease-stage-progress); }


.st-point:is([data-state=active], [data-state=held], [data-state=done]) .st-dot {
  background: var(--st-on);
  box-shadow: inset 0 0 0 var(--stage-dot-ring) var(--st-on);
  animation: st-arrive var(--duration-stage-arrive) var(--ease-stage-arrive) var(--st-arrive-delay);
}
.st-point[data-state=error] .st-dot {
  background: var(--st-error);
  box-shadow: inset 0 0 0 var(--stage-dot-ring) var(--st-error);
  animation: st-arrive var(--duration-stage-arrive) var(--ease-stage-arrive) var(--st-arrive-delay);
}
@keyframes st-arrive { 40% { transform: scale(var(--scale-stage-arrive)); } }


.st-point[data-state=active][data-kind=pulse] .st-halo {
  animation: st-halo var(--duration-stage-pulse) var(--ease-stage-pulse) var(--st-halo-delay) infinite;
}

.st-point[data-landed] .st-halo {
  animation: st-halo var(--duration-stage-pulse) var(--ease-stage-pulse) var(--st-halo-delay) 1;
}
@keyframes st-halo {
  0% { opacity: var(--opacity-stage-halo); transform: scale(1); }
  60%, 100% { opacity: 0; transform: scale(var(--scale-stage-halo)); }
}


.st-point[data-kind=spin] .st-ring-track { stroke: var(--transparent); fill: none; }
.st-point[data-kind=spin] .st-ring-arc { stroke-dasharray: 28 100; transition: none; }
.st-point[data-kind=spin] .st-ring { transition: opacity var(--duration-fast) var(--ease); }
.st-point[data-kind=spin][data-state=active] .st-ring {
  opacity: 1;
  transition-delay: var(--st-arrive-delay);
  animation: st-spin var(--duration-stage-spin) var(--ease-stage-spin) infinite;
}
@keyframes st-spin { to { transform: rotate(1turn); } }


.st-point[data-kind=ring] .st-ring { opacity: 1; transition: opacity var(--duration-stage-arrive) var(--ease-expand), transform var(--duration-stage-arrive) var(--ease-expand); }
.st-point[data-kind=ring] .st-dot { opacity: 0; transform: scale(.4); animation: none; transition: opacity var(--duration-fast) var(--ease), transform var(--duration-stage-arrive) var(--ease-stage-arrive); }
.st-point[data-kind=ring][data-state=done] .st-ring { opacity: 0; transform: scale(.5); }
.st-point[data-kind=ring][data-state=done] .st-dot { opacity: 1; transform: none; animation: none; transition-delay: var(--duration-label-out); }


.st-label { font-size: var(--fs-xs); line-height: var(--lh-tight); color: var(--muted); transition: color var(--duration-fast) var(--ease) var(--st-arrive-delay); }
.st-point:is([data-state=active], [data-state=held]) .st-label { color: var(--text); }
.st-point[data-state=done] .st-label { color: var(--secondary); }
.st-point[data-state=error] .st-label { color: var(--danger); }



.st--compact { display: flex; align-items: center; gap: var(--s4); margin-left: calc((var(--stage-dot) - var(--stage-box)) / 2); }
.st--compact .st-points { display: flex; gap: var(--stage-gap); }
.st--compact .st-line { left: calc(var(--st-box) / 2); right: calc(var(--st-box) / 2); }
.st--compact .st-label { position: absolute; width: var(--hairline); height: var(--hairline); overflow: hidden; clip-path: inset(50%); white-space: nowrap; }
.st-status { font-size: var(--fs-xs); line-height: var(--lh-tight); color: var(--secondary); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; min-width: var(--s0); font-variant-numeric: tabular-nums; transition: color var(--duration-fast) var(--ease); }
.st-status[data-tone=quiet] { color: var(--muted); }
.st-status[data-tone=error] { color: var(--danger); }


.st--labeled {
  --st-box: var(--stage-box-large);
  --st-dot: var(--stage-dot-large);
  --st-line: var(--stage-line-large);
  --st-fill-duration: var(--duration-stage-fill);
}
.st--labeled .st-points { display: grid; grid-template-columns: repeat(var(--st-count), minmax(var(--s0), 1fr)); }
.st--labeled .st-point { flex-direction: column; gap: var(--s2); text-align: center; }
.st--labeled .st-label { min-height: var(--stage-label-lines); padding-inline: var(--s1); overflow-wrap: anywhere; }
.st--labeled .st-line { left: calc(50% / var(--st-count)); right: calc(50% / var(--st-count)); }


@media (prefers-reduced-motion: reduce) {
  .st * { animation: none !important; transition: none !important; }
  .st-point[data-state=active] .st-dot { box-shadow: inset 0 0 0 var(--stage-dot-ring) var(--st-on), 0 0 0 var(--stage-dot-ring) var(--st-surface), 0 0 0 var(--s2) var(--st-on); }
  .st-point[data-kind=spin][data-state=active] .st-ring { opacity: 0; }
}
.st[data-instant] * { animation: none !important; transition: none !important; }


.st-error { color: var(--danger); font-size: var(--fs-xs); margin-top: var(--s4); }
</style>
