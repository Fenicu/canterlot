<script lang="ts">
  import { resolve } from '$app/paths';
  import { fmt } from '#lib/format/ru.js';
  import { modelLabel } from '#lib/format/labels.js';
  import Icon from '#lib/ui/Icon.svelte';
  import { timeTip } from '#lib/ui/time-tip.js';
  import type { ListRow } from '#lib/stores/list.svelte.js';
  import { activityLine, type Activity } from './activityText';
  import { colorVar, toneVar } from './colors';
  import { getRuntime } from '#lib/app/runtime.js';
  import { planBadge } from '#lib/screens/plan/planText.js';

  let {
    item,
    now,
    current = false,
    depth = 0,
    onopen
  }: {
    item: ListRow;
    now: number;
    current?: boolean;
    depth?: number;
    onopen: (id: string) => void;
  } = $props();

  const line = $derived(activityLine(item.activity as Activity, { now }));
  function planOf(id: string) {
    try {
      return getRuntime().list.plan.of(id);
    } catch {
      return [];
    }
  }
  // Метка плана у оркестратора: сколько задач готово из всех неотменённых.
  const plan = $derived(planBadge(planOf(item.id)));
  // Как в шапке чата: точка показывает состояние, а цвет владельца — только у спокойной сессии.
  const mark = $derived(line.tone === 'default' && item.color ? colorVar(item.color) : toneVar(line.tone));
  const stampTs = $derived.by(() => {
    const parsed = item.last_activity_at ? Date.parse(item.last_activity_at) : Number.NaN;
    return Number.isFinite(parsed) ? parsed : null;
  });
  const stamp = $derived(stampTs !== null ? fmt.clock(stampTs) : '');
  const tail = $derived(item.unread_count > 0 && stamp !== '');
  // Разделитель собирается в скрипте: Svelte срезает пробелы в начале и конце блока.
  const stampSeparator = ' · ';
  let timer = 0;
  let held = false;

  function down() {
    held = false;
    timer = window.setTimeout(() => {
      held = true;
      onopen(item.id);
    }, 500);
  }
  function up() {
    window.clearTimeout(timer);
  }
  function click(event: MouseEvent) {
    if (!held) return;
    event.preventDefault();
    held = false;
  }
</script>

<article class="row" class:current class:depth-1={depth === 1} class:depth-2={depth >= 2} data-session={item.id} oncontextmenu={(event) => { event.preventDefault(); onopen(item.id); }}>
  <a href={resolve(`/c/${item.id}` as '/')} aria-current={current ? 'page' : undefined} onpointerdown={down} onpointerup={up} onpointercancel={up} onclick={click}>
    <span class="mark" data-token={mark}></span>
    <span class="title" title={item.title}>{item.title}</span>
    <span class="side">
      {#if item.unread_count > 1}
        <span class="sr-only">Непрочитанных: {item.unread_count}</span><span class="count" aria-hidden="true">{item.unread_count}</span>
      {:else if item.unread_count === 1}
        <span class="sr-only">Есть непрочитанное</span><span class="unread" aria-hidden="true"></span>
      {:else if stamp && stampTs !== null}
        <time datetime={item.last_activity_at ?? undefined} use:timeTip={stampTs}>{stamp}</time>
      {/if}
    </span>
    <span class="meta">{item.harness.title} · {modelLabel(item.model)} · {item.machine.name}</span>
    <span class="activity" data-tone={line.tone}>{#if line.live}<span class="live" aria-hidden="true"></span>{/if}{line.text}{#if tail && stampTs !== null}{stampSeparator}<time datetime={item.last_activity_at ?? undefined} use:timeTip={stampTs}>{stamp}</time>{/if}</span>
    {#if plan}
      <span class="plan" data-role="plan-badge"><span class="sr-only">{plan.label}</span><span aria-hidden="true">{plan.text}</span></span>
    {/if}
    {#if depth > 0 && item.working_ms}
      <span class="worked" data-worked>работала {fmt.durationCoarse(item.working_ms)}</span>
    {/if}
  </a>
  <button type="button" aria-label={`Действия: ${item.title}`} onpointerdown={(event) => event.stopPropagation()} onclick={() => onopen(item.id)}>
    <Icon name="more" />
  </button>
</article>

<style>
  .row { display: flex; align-items: flex-start; gap: var(--s2); margin-inline: var(--s3); padding: var(--s4) var(--s3) var(--s4) var(--s6); border-radius: var(--radius-card); }
  .row.depth-1 { margin-left: calc(var(--s3) + var(--s9)); }
  .row.depth-2 { margin-left: calc(var(--s3) + var(--s10) + var(--s4)); }
  .row.current { background: var(--selected); }
  .row.current .count { background: var(--surface); }
  @media (hover: hover) and (pointer: fine) {
    .row:not(.current):hover { background: var(--hover); }
    .row:not(.current):not(:hover):not(:focus-within) button { opacity: 0; }
  }
  a { flex: 1; min-width: 0; display: grid; grid-template-columns: var(--mark) minmax(0, 1fr) auto; gap: var(--s1) var(--s4); border-radius: var(--radius-sm); color: inherit; }
  .mark { width: var(--mark); height: var(--mark); margin-top: calc((var(--fs-body) * var(--lh-body) - var(--mark)) / 2); border-radius: var(--radius-round); background: var(--muted); }
  .mark[data-token='--accent'] { background: var(--accent); }
  .mark[data-token='--blue'] { background: var(--blue); }
  .mark[data-token='--warning'] { background: var(--warning); }
  .mark[data-token='--success'] { background: var(--success); }
  .mark[data-token='--danger'] { background: var(--danger); }
  .title { display: -webkit-box; overflow: hidden; -webkit-box-orient: vertical; -webkit-line-clamp: 2; line-clamp: 2; overflow-wrap: anywhere; }
  .side { display: inline-flex; align-items: center; justify-content: flex-end; height: calc(var(--fs-body) * var(--lh-body)); }
  time { color: var(--time); font-size: var(--fs-sm); font-variant-numeric: tabular-nums; }
  .meta, .activity { grid-column: 2 / -1; min-width: 0; overflow: hidden; color: var(--secondary); font-size: var(--fs-sm); text-overflow: ellipsis; white-space: nowrap; }
  .plan { grid-column: 2 / -1; justify-self: start; max-width: 100%; margin-top: var(--s1); padding: var(--s0) var(--s3); overflow: hidden; border-radius: var(--radius-pill); background: var(--chip); color: var(--chip-text); font-size: var(--fs-sm); font-variant-numeric: tabular-nums; text-overflow: ellipsis; white-space: nowrap; }
  .worked { grid-column: 2 / -1; min-width: 0; overflow: hidden; color: var(--muted); font-size: var(--fs-sm); font-variant-numeric: tabular-nums; text-overflow: ellipsis; white-space: nowrap; }
  .activity[data-tone='accent'] { color: var(--accent-link); }
  .activity[data-tone='warning'] { color: var(--warning); }
  .activity[data-tone='danger'] { color: var(--danger); }
  .unread { width: var(--mark); height: var(--mark); border-radius: var(--radius-round); background: var(--text); }
  .count { min-width: var(--s10); padding: var(--s0) var(--s3); border-radius: var(--radius-pill); background: var(--chip); color: var(--chip-text); font-size: var(--fs-sm); text-align: center; }
  .live { display: inline-block; width: var(--mark); height: var(--mark); margin-right: var(--s3); border-radius: var(--radius-round); background: var(--accent); }
  button { flex: none; min-width: var(--target); min-height: var(--target); margin-top: calc((var(--fs-body) * var(--lh-body) - var(--target)) / 2); display: inline-flex; align-items: center; justify-content: center; border: var(--s0); background: var(--transparent); color: var(--secondary); }
</style>
