<script lang="ts">
  import SubagentRow from '#lib/timeline/SubagentRow.svelte';
  import type { CardProps } from './types';

  let { call, env }: CardProps<Record<string, never>> = $props();
</script>

<!-- Субагент Claude Code и Codex: строка ведёт в его ветку, вид прежний. -->
<SubagentRow call={call.block} nodes={env.links.children(call.block.id)} result={call.result} background={env.background} working={env.working} now={env.now} conversationId={env.conversationId} />
