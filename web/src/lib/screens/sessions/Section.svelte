<script lang="ts">
  import type { ListRow } from '$lib/stores/list.svelte';
  import SessionRow from './SessionRow.svelte';

  let {
    id,
    title,
    count,
    items,
    collapsed,
    now,
    ontoggle,
    onopen
  }: {
    id: string;
    title: string;
    count: number;
    items: ListRow[];
    collapsed: boolean;
    now: number;
    ontoggle: (id: string) => void;
    onopen: (id: string) => void;
  } = $props();
</script>

<section>
  <h2>
    <button type="button" aria-expanded={!collapsed} onclick={() => ontoggle(id)}>{title} {count}</button>
  </h2>
  {#if !collapsed}
    {#each items as item (item.id)}
      <SessionRow {item} {now} {onopen} />
    {/each}
  {/if}
</section>

<style>
  h2 { margin: 0; font-size: var(--fs-meta); font-weight: var(--weight-medium); }
  button {
    width: 100%;
    min-height: var(--target);
    text-align: left;
    border: var(--s0);
    background: var(--transparent);
    color: var(--secondary);
    padding: var(--s2) var(--s6);
  }
</style>
