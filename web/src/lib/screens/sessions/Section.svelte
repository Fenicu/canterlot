<script lang="ts">
  import type { TreeNode } from './grouping';
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
    ontoggleKids,
    onopen
  }: {
    id: string;
    title: string;
    count: number;
    items: TreeNode[];
    collapsed: boolean;
    now: number;
    current?: string;
    heading?: boolean;
    ontoggle: (id: string) => void;
    ontoggleKids: (id: string) => void;
    onopen: (id: string) => void;
  } = $props();
</script>

{#snippet branch(node: TreeNode)}
  {#if node.children.length > 0}
    {@const label = node.open
      ? `Свернуть делегированные: ${node.children.length}`
      : `Развернуть делегированные: ${node.children.length}`}
    <button
      type="button"
      class="kids depth-{Math.min(node.depth + 1, 2)}"
      aria-expanded={node.open}
      aria-label={label}
      onclick={() => ontoggleKids(node.item.id)}
    >
      <span class="stem" aria-hidden="true">└</span>
      <span class="kids-title">Делегированные · {node.children.length}{#if node.running > 0} · работает {node.running}{/if}</span>
      <span class="chevron" class:open={node.open}><Icon name="chevron" /></span>
    </button>
    {#if node.open}
      {#each node.children as child (child.item.id)}
        <SessionRow item={child.item} {now} depth={child.depth} current={child.item.id === current} {onopen} />
        {@render branch(child)}
      {/each}
    {/if}
  {/if}
{/snippet}

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
    {#each items as node (node.item.id)}
      <SessionRow item={node.item} {now} depth={node.depth} current={node.item.id === current} {onopen} />
      {@render branch(node)}
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
  .kids {
    min-height: var(--target);
    color: var(--muted);
    font-size: var(--fs-meta);
  }
  .kids.depth-1 { padding-left: calc(var(--gutter) + var(--s9)); }
  .kids.depth-2 { padding-left: calc(var(--gutter) + var(--s10) + var(--s4)); }
  .stem { color: var(--accent-link); font-family: var(--font-mono); }
  .kids-title { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
</style>
