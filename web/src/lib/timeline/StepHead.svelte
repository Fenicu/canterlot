<script lang="ts">
  import type { Snippet } from 'svelte';
  import StatusPill from '#lib/timeline/StatusPill.svelte';
  import Icon from '#lib/ui/Icon.svelte';

  let {
    name,
    detail = '',
    dir = '',
    status = '',
    state = 'done',
    open,
    ontoggle,
    badge
  }: {
    name: string;
    detail?: string;
    dir?: string;
    status?: string;
    state?: 'running' | 'done' | 'error';
    open: boolean;
    ontoggle: () => void;
    badge?: Snippet;
  } = $props();
</script>

<button type="button" class="head" aria-expanded={open} onclick={ontoggle}>
  <span class="chevron" data-open={open ? '' : undefined}><Icon name="chevron" /></span>
  <span class="name">{name}</span>
  {#if detail}<span class="summary">{detail}{#if dir}<span class="dir">{dir}</span>{/if}</span>{/if}
  {@render badge?.()}
  {#if status}<StatusPill {state} text={status} />{/if}
</button>

<style>
  .head {
    display: flex;
    gap: var(--s3);
    align-items: center;
    width: 100%;
    min-height: var(--target);
    padding: var(--s2) var(--s5) var(--s2) var(--s4);
    border-radius: var(--radius-card);
    background: var(--transparent);
    font-size: var(--fs-sm);
    text-align: left;
  }
  .chevron { flex: none; color: var(--muted); transition: transform var(--duration-expand) var(--ease-expand); }
  .chevron :global(svg) { width: var(--icon-small); height: var(--icon-small); }
  .chevron[data-open] { transform: rotate(90deg); }
  .name { flex: 0 1 auto; min-width: 0; max-width: 70%; overflow: hidden; font-family: var(--font-mono); font-weight: var(--weight-semibold); text-overflow: ellipsis; white-space: nowrap; }
  .dir { margin-left: var(--s3); color: var(--muted); }
  .summary { flex: 1 1 0; min-width: 0; overflow: hidden; color: var(--secondary); font-family: var(--font-mono); text-overflow: ellipsis; white-space: nowrap; }
</style>
