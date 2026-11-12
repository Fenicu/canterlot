<script lang="ts">
  import { resolve } from '$app/paths';
  import Icon from '#lib/ui/Icon.svelte';
  import type { IconName } from '#lib/ui/icons.js';
  import type { Tab } from './grouping';
  import { groupIcon } from './groupIcons';

  let {
    tabs,
    current,
    icons,
    counted = true,
    section,
    onselect
  }: {
    tabs: { tab: Tab; label: string; count: number }[];
    current: Tab;
    /** Значки групп по идентификатору. */
    icons: Record<string, string | null | undefined>;
    counted?: boolean;
    /** Открытый раздел приложения: подсвечивает его внизу полосы. */
    section: 'sessions' | 'machines' | 'limits' | 'presets';
    onselect: (tab: Tab) => void;
  } = $props();

  // Мягкий перенос держит длинные подписи в две строки, как «Закреп-лённые».
  const SHORT: Record<string, string> = { pinned: 'Закреп­лённые' };
  const FIXED: Record<string, IconName> = { all: 'chats', pinned: 'pin', archive: 'archive', trash: 'trash' };

  function same(a: Tab, b: Tab): boolean {
    if (a.kind !== b.kind) return false;
    return a.kind !== 'group' || b.kind !== 'group' || a.id === b.id;
  }

  function iconOf(tab: Tab): IconName {
    return tab.kind === 'group' ? groupIcon(icons[tab.id]) : FIXED[tab.kind] ?? 'folder';
  }

  let list = $state<HTMLElement | null>(null);

  function move(event: KeyboardEvent) {
    const forward = event.key === 'ArrowDown' || event.key === 'ArrowRight';
    const back = event.key === 'ArrowUp' || event.key === 'ArrowLeft';
    if (!forward && !back) return;
    event.preventDefault();
    const index = tabs.findIndex((item) => same(item.tab, current));
    const next = tabs[(index + (forward ? 1 : -1) + tabs.length) % tabs.length];
    if (!next) return;
    onselect(next.tab);
    queueMicrotask(() => list?.querySelector<HTMLElement>('[aria-selected="true"]')?.focus());
  }
</script>

<div class="rail" data-group-rail>
  <div bind:this={list} class="tabs" role="tablist" aria-label="Группы сессий" aria-orientation="vertical" tabindex="-1" onkeydown={move}>
    {#each tabs as item, index (item.label + index)}
      {@const selected = same(item.tab, current)}
      <button
        type="button"
        role="tab"
        class="item"
        aria-selected={selected}
        tabindex={selected ? 0 : -1}
        data-tab={item.tab.kind}
        title={item.label}
        aria-label={counted ? `${item.label} ${item.count}` : item.label}
        onclick={() => onselect(item.tab)}
      >
        <span class="icon"><Icon name={iconOf(item.tab)} /></span>
        <span class="label">{SHORT[item.tab.kind] ?? item.label}</span>
        {#if counted && item.count > 0}<span class="count">{item.count}</span>{/if}
      </button>
    {/each}
  </div>
  <span class="sep" aria-hidden="true"></span>
  <nav class="sections" aria-label="Разделы">
    <a class="item" href={resolve('/presets')} aria-current={section === 'presets' ? 'page' : undefined}><span class="icon"><Icon name="doc" /></span><span class="label">Инструк&shy;ции</span></a>
    <a class="item" href={resolve('/machines')} aria-current={section === 'machines' ? 'page' : undefined}><span class="icon"><Icon name="server" /></span><span class="label">Машины</span></a>
    <a class="item" href={resolve('/limits')} aria-current={section === 'limits' ? 'page' : undefined}><span class="icon"><Icon name="gauge" /></span><span class="label">Лимиты</span></a>
  </nav>
</div>

<style>
  .rail { flex: 0 0 var(--rail-width); width: var(--rail-width); min-height: 0; display: flex; flex-direction: column; padding-block: var(--s4) var(--s4); background: var(--bg); }
  .tabs { flex: 1; min-height: 0; overflow-y: auto; scrollbar-width: none; display: flex; flex-direction: column; }
  .sep { flex: none; height: var(--hairline); margin: var(--s3) var(--s7); background: var(--line); }
  .sections { flex: none; display: flex; flex-direction: column; }
  .item { position: relative; display: flex; flex-direction: column; align-items: center; gap: var(--s2); width: 100%; padding: var(--s5) var(--s2) var(--s4); border: var(--s0); background: var(--transparent); color: var(--muted); font-size: var(--fs-xs); line-height: var(--lh-tight); text-align: center; }
  .icon { display: inline-flex; color: var(--muted); }
  .icon :global(svg) { width: var(--s10); height: var(--s10); }
  .label { max-width: 100%; display: -webkit-box; overflow: hidden; -webkit-box-orient: vertical; -webkit-line-clamp: 2; line-clamp: 2; overflow-wrap: anywhere; hyphens: manual; }
  .count { position: absolute; top: var(--s2); left: calc(50% + var(--s2)); min-width: var(--s9); height: var(--s9); box-sizing: border-box; display: inline-flex; align-items: center; justify-content: center; padding: var(--s0) var(--s2); border-radius: var(--radius-pill); background: var(--badge); color: var(--bg); font-size: var(--fs-xs); font-weight: var(--weight-bold); font-variant-numeric: tabular-nums; }
  @media (hover: hover) and (pointer: fine) {
    .item:hover { background: var(--hover); color: var(--text); }
  }
  .item[aria-selected='true'], .item[aria-current='page'] { background: var(--selected); color: var(--accent-link); }
  .item[aria-selected='true'] .icon, .item[aria-current='page'] .icon { color: var(--accent-link); }
  .item[aria-selected='true'] .count { background: var(--accent); color: var(--accent-ink); }
  .item:focus-visible { outline: var(--focus-width) solid var(--accent); outline-offset: calc(var(--focus-width) * -1); }
</style>
