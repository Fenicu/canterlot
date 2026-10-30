<script lang="ts">
  import type { ListRow } from '#lib/stores/list.svelte.js';
  import Icon from '#lib/ui/Icon.svelte';
  import SessionRow from './SessionRow.svelte';

  let {
    id,
    title,
    count,
    items,
    collapsed,
    now,
    current = '',
    heading = true,
    ontoggle,
    onopen
  }: {
    id: string;
    title: string;
    count: number;
    items: ListRow[];
    collapsed: boolean;
    now: number;
    current?: string;
    heading?: boolean;
    ontoggle: (id: string) => void;
    onopen: (id: string) => void;
  } = $props();
</script>

<section>
  {#if heading}
    <h2>
      <button type="button" aria-expanded={!collapsed} onclick={() => ontoggle(id)}>
        <span class="chevron" class:open={!collapsed}><Icon name="chevron" /></span>
        <span class="title">{title}</span>
        <span class="count">{count}</span>
      </button>
    </h2>
  {/if}
  {#if !heading || !collapsed}
    {#each items as item (item.id)}
      <SessionRow {item} {now} current={item.id === current} {onopen} />
    {/each}
  {/if}
</section>

<style>
  h2 { margin: 0; font-size: var(--fs-meta); font-weight: var(--weight-medium); }
  button {
    display: flex;
    align-items: center;
    gap: var(--s2);
    width: 100%;
    min-height: var(--target);
    text-align: left;
    border: var(--s0);
    background: var(--transparent);
    color: var(--secondary);
    padding: var(--s2) var(--gutter);
  }
  .chevron { flex: none; color: var(--muted); transition: transform var(--duration-fast) var(--ease); }
  .chevron :global(svg) { width: var(--icon-small); height: var(--icon-small); }
  .chevron.open { transform: rotate(90deg); }
  .title { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .count { flex: none; color: var(--muted); font-variant-numeric: tabular-nums; }
</style>
