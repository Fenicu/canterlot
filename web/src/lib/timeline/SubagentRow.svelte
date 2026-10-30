<script lang="ts">
  import { resolve } from '$app/paths';
  import type { BlockModel } from '#lib/stores/blocks.js';
  import { subagentCopy } from '#lib/timeline/present.js';
  import Icon from '#lib/ui/Icon.svelte';
  let { call, nodes, conversationId }: { call: BlockModel; nodes: BlockModel[]; conversationId: string } = $props();
  const copy = $derived(subagentCopy(call, nodes));
</script>

<a class="sub" href={resolve(`/c/${conversationId}/lane/${call.id}` as '/')}>
  <span class="branch"><Icon name="branch" /></span>
  <span class="text">
    <span class="title">{copy.title}</span>
    <span class="status">{copy.detail}</span>
  </span>
  <span class="chevron"><Icon name="chevron" /></span>
</a>

<style>
  .sub { display: flex; align-items: center; gap: var(--s5); min-width: 0; min-height: var(--target); padding: var(--s3) var(--s5) var(--s3) var(--s6); border-radius: var(--radius-card); background: var(--raised); color: inherit; }
  .branch { flex: none; color: var(--accent-link); }
  .text { display: flex; flex: 1; flex-direction: column; gap: var(--s1); min-width: 0; }
  .title, .status { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .title { font-size: var(--fs-meta); }
  .status { color: var(--accent-link); font-size: var(--fs-sm); }
  .chevron { flex: none; color: var(--secondary); }
  .chevron :global(svg) { width: var(--icon-small); height: var(--icon-small); }
</style>
