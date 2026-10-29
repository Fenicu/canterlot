<script lang="ts">
  import { resolve } from '$app/paths';
  import type { BlockModel } from '$lib/stores/blocks';
  import { subagentCopy } from '$lib/timeline/present';
  let { call, nodes, conversationId }: { call: BlockModel; nodes: BlockModel[]; conversationId: string } = $props();
  const copy = $derived(subagentCopy(call, nodes));
</script>

<a class="sub" href={resolve(`/c/${conversationId}/lane/${call.id}` as '/')}>
  <span>{copy.title}</span>
  <span class="status">{copy.detail}</span>
</a>

<style>
  .sub { display: flex; flex-direction: column; gap: var(--s1); color: inherit; }
  .status { color: var(--accent-link); }
</style>
