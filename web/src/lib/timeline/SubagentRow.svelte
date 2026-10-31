<script lang="ts">
  import { resolve } from '$app/paths';
  import type { BackgroundFact } from '#lib/stores/background.svelte.js';
  import type { BlockModel } from '#lib/stores/blocks.js';
  import { subagentCopy, toolPillText, toolTiming } from '#lib/timeline/present.js';
  import StatusPill from '#lib/timeline/StatusPill.svelte';
  import Icon from '#lib/ui/Icon.svelte';
  let {
    call,
    nodes,
    result,
    background = null,
    working,
    now,
    conversationId
  }: {
    call: BlockModel;
    nodes: BlockModel[];
    result: BlockModel | null;
    background?: BackgroundFact | null;
    working: boolean;
    now: number;
    conversationId: string;
  } = $props();
  const copy = $derived(subagentCopy(call, nodes, { result, working, background }));
  const pill = $derived(toolPillText({ ...toolTiming(call, result, working, background), error: copy.state === 'error', now }));
</script>

<a class="sub" href={resolve(`/c/${conversationId}/lane/${call.id}` as '/')}>
  <span class="branch"><Icon name="branch" /></span>
  <span class="text">
    <span class="title">{copy.title}</span>
    <span class="status" data-state={copy.state}>{copy.detail}</span>
  </span>
  {#if pill && copy.state !== 'stopped'}<StatusPill state={copy.state} text={pill} />{/if}
  <span class="chevron"><Icon name="chevron" /></span>
</a>

<style>
  .sub { display: flex; align-items: center; gap: var(--s5); min-width: 0; min-height: var(--target); padding: var(--s3) var(--s5) var(--s3) var(--s6); border-radius: var(--radius-card); background: var(--raised); color: inherit; }
  .branch { flex: none; color: var(--accent-link); }
  .text { display: flex; flex: 1; flex-direction: column; gap: var(--s1); min-width: 0; }
  .title, .status { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .title { font-size: var(--fs-meta); }
  .status { color: var(--secondary); font-size: var(--fs-sm); }
  .status[data-state='running'] { color: var(--accent-link); }
  .status[data-state='error'] { color: var(--danger); }
  .chevron { flex: none; color: var(--secondary); }
  .chevron :global(svg) { width: var(--icon-small); height: var(--icon-small); }
</style>
