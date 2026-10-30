<script lang="ts">
  import type { Tab } from './grouping';

  let {
    tabs,
    current,
    onselect
  }: {
    tabs: { tab: Tab; label: string; count: number }[];
    current: Tab;
    onselect: (tab: Tab) => void;
  } = $props();

  function same(a: Tab, b: Tab): boolean {
    if (a.kind !== b.kind) return false;
    return a.kind !== 'group' || b.kind !== 'group' || a.id === b.id;
  }

  function move(event: KeyboardEvent) {
    const forward = event.key === 'ArrowRight' || event.key === 'ArrowDown';
    const back = event.key === 'ArrowLeft' || event.key === 'ArrowUp';
    if (!forward && !back) return;
    event.preventDefault();
    const index = tabs.findIndex((item) => same(item.tab, current));
    const next = tabs[(index + (forward ? 1 : -1) + tabs.length) % tabs.length];
    if (next) onselect(next.tab);
  }
</script>

<div class="tabs" role="tablist" aria-label="Группы сессий" tabindex="-1" onkeydown={move}>
  {#each tabs as item, index (item.label + index)}
    <button
      type="button"
      role="tab"
      aria-selected={same(item.tab, current)}
      tabindex={same(item.tab, current) ? 0 : -1}
      onclick={() => onselect(item.tab)}
    >
      {item.label} {item.count}
    </button>
  {/each}
</div>

<style>
  .tabs {
    display: flex;
    gap: var(--s2);
    min-width: 0;
    overflow-x: auto;
    scrollbar-width: none;
    margin: var(--s3) var(--s6);
    padding: var(--s2);
    border-radius: var(--radius-pill);
    background: var(--raised);
  }
  button {
    flex: 1 0 auto;
    min-height: var(--target);
    border: var(--s0);
    border-radius: var(--radius-pill);
    background: var(--transparent);
    color: var(--muted);
    font-size: var(--fs-meta);
    padding: var(--s4) var(--s6);
  }
  button[aria-selected='true'] { background: var(--selected); color: var(--accent-link); }
  :global([data-pane='list']) .tabs { flex-wrap: wrap; overflow-x: visible; border-radius: var(--radius-card); }
  :global([data-pane='list']) button { flex-shrink: 1; min-width: 0; max-width: 100%; overflow-wrap: anywhere; }
</style>
