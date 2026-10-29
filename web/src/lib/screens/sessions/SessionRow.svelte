<script lang="ts">
  import { resolve } from '$app/paths';
  import { fmt } from '$lib/format/ru';
  import Icon from '$lib/ui/Icon.svelte';
  import type { ListRow } from '$lib/stores/list.svelte';
  import { activityLine, type Activity } from './activityText';
  import { colorVar } from './colors';

  let {
    item,
    now,
    onopen
  }: {
    item: ListRow;
    now: number;
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

<article class="row" data-session={item.id} oncontextmenu={(event) => { event.preventDefault(); onopen(item.id); }}>
  <a href={resolve(`/c/${item.id}` as '/')} onpointerdown={down} onpointerup={up} onpointercancel={up} onclick={click}>
    <span class="mark" data-token={colorVar(item.color)}></span>
    <span class="title">{item.title}</span>
    {#if item.unread_count > 0}<span class="sr-only">Есть непрочитанное</span><span class="unread" aria-hidden="true"></span>{/if}
    {#if stamp}<time datetime={item.last_activity_at ?? undefined}>{stamp}</time>{/if}
    <span class="meta">{item.harness.title} · {item.model} · {item.machine.name}</span>
    <span class="activity" data-tone={line.tone}>{#if line.live}<span class="live" aria-hidden="true"></span>{/if}{line.text}</span>
  </a>
  <button type="button" aria-label={`Действия: ${item.title}`} onpointerdown={(event) => event.stopPropagation()} onclick={() => onopen(item.id)}>
    <Icon name="more" />
  </button>
</article>

<style>
  .row { display: flex; align-items: flex-start; gap: var(--s2); padding: var(--s3) var(--s4); }
  a { flex: 1; min-width: 0; display: grid; grid-template-columns: var(--mark) 1fr auto; gap: var(--s2) var(--s3); color: inherit; }
  .mark { width: var(--mark); height: var(--mark); margin-top: var(--s2); border-radius: var(--radius-round); background: var(--muted); }
  .mark[data-token='--accent'] { background: var(--accent); }
  .mark[data-token='--blue'] { background: var(--blue); }
  .mark[data-token='--warning'] { background: var(--warning); }
  .mark[data-token='--success'] { background: var(--success); }
  .mark[data-token='--danger'] { background: var(--danger); }
  .title { overflow-wrap: anywhere; }
  time, .meta, .activity { color: var(--secondary); font-size: var(--fs-sm); }
  .meta, .activity { grid-column: 2 / -1; }
  .activity[data-tone='accent'] { color: var(--accent-link); }
  .activity[data-tone='warning'] { color: var(--warning); }
  .activity[data-tone='danger'] { color: var(--danger); }
  .unread, .live { width: var(--mark); height: var(--mark); border-radius: var(--radius-round); background: var(--accent); display: inline-block; }
  button { min-width: var(--target); min-height: var(--target); border: var(--s0); background: var(--transparent); color: var(--text); }
</style>
