<script lang="ts">
  import { resolve } from '$app/paths';
  import { fmt } from '#lib/format/ru.js';
  import { modelLabel } from '#lib/screens/chat/settingsLabels.js';
  import Icon from '#lib/ui/Icon.svelte';
  import type { ListRow } from '#lib/stores/list.svelte.js';
  import { activityLine, type Activity } from './activityText';
  import { colorVar } from './colors';

  let {
    item,
    now,
    current = false,
    onopen
  }: {
    item: ListRow;
    now: number;
    current?: boolean;
    onopen: (id: string) => void;
  } = $props();

  const line = $derived(activityLine(item.activity as Activity, { now }));
  const stamp = $derived(item.last_activity_at ? fmt.clock(Date.parse(item.last_activity_at)) : '');
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

<article class="row" class:current data-session={item.id} oncontextmenu={(event) => { event.preventDefault(); onopen(item.id); }}>
  <a href={resolve(`/c/${item.id}` as '/')} aria-current={current ? 'page' : undefined} onpointerdown={down} onpointerup={up} onpointercancel={up} onclick={click}>
    <span class="mark" data-token={colorVar(item.color)}></span>
    <span class="title">{item.title}</span>
    <span class="side">
      {#if item.unread_count > 1}
        <span class="sr-only">Непрочитанных: {item.unread_count}</span><span class="count" aria-hidden="true">{item.unread_count}</span>
      {:else if item.unread_count === 1}
        <span class="sr-only">Есть непрочитанное</span><span class="unread" aria-hidden="true"></span>
      {:else if stamp}
        <time datetime={item.last_activity_at ?? undefined}>{stamp}</time>
      {/if}
    </span>
    <span class="meta">{item.harness.title} · {modelLabel(item.model)} · {item.machine.name}</span>
    <span class="activity" data-tone={line.tone}>{#if line.live}<span class="live" aria-hidden="true"></span>{/if}{line.text}</span>
  </a>
  <button type="button" aria-label={`Действия: ${item.title}`} onpointerdown={(event) => event.stopPropagation()} onclick={() => onopen(item.id)}>
    <Icon name="more" />
  </button>
</article>

<style>
  .row { display: flex; align-items: flex-start; gap: var(--s2); margin-inline: var(--s3); padding: var(--s4) var(--s3) var(--s4) var(--s6); border-radius: var(--radius-card); }
  .row.current { background: var(--selected); }
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
  .activity { color: var(--text); }
  .activity[data-tone='accent'] { color: var(--accent-link); }
  .activity[data-tone='warning'] { color: var(--warning); }
  .activity[data-tone='danger'] { color: var(--danger); }
  .unread { width: var(--mark); height: var(--mark); border-radius: var(--radius-round); background: var(--text); }
  .count { min-width: var(--s10); padding: var(--s0) var(--s3); border-radius: var(--radius-pill); background: var(--chip); color: var(--chip-text); font-size: var(--fs-sm); text-align: center; }
  .live { display: inline-block; width: var(--mark); height: var(--mark); margin-right: var(--s3); border-radius: var(--radius-round); background: var(--accent); }
  button { flex: none; min-width: var(--target); min-height: var(--target); margin-top: calc((var(--fs-body) * var(--lh-body) - var(--target)) / 2); display: inline-flex; align-items: center; justify-content: center; border: var(--s0); background: var(--transparent); color: var(--secondary); }
</style>
