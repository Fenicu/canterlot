<script lang="ts">
  import { fmt } from '#lib/format/ru.js';
  import type { BlockPage } from '#lib/stores/blocks.js';
  import type { TimelineEntry } from '#lib/stores/timeline.js';
  import { childBlocks } from '#lib/timeline/rows.js';
  import LiveLine from '#lib/timeline/LiveLine.svelte';
  import NoticeBlock from '#lib/timeline/NoticeBlock.svelte';
  import ReasoningBlock from '#lib/timeline/ReasoningBlock.svelte';
  import SubagentRow from '#lib/timeline/SubagentRow.svelte';
  import TextBlock from '#lib/timeline/TextBlock.svelte';
  import ToolBlock from '#lib/timeline/ToolBlock.svelte';
  import IconButton from '#lib/ui/IconButton.svelte';

  let {
    entries,
    all,
    conversationId,
    now,
    onInfo,
    load
  }: {
    entries: TimelineEntry[];
    all: TimelineEntry[];
    conversationId: string;
    now: number;
    onInfo?: (runId: string) => void;
    load: (blockId: string, offset: number) => Promise<BlockPage>;
  } = $props();

  let expanded = $state(false);
  const blocks = $derived(entries.flatMap((entry) => (entry.kind === 'block' ? [entry.block] : [])));
  const hidden = $derived(Math.max(0, blocks.length - 40));
  const shown = $derived(!expanded && blocks.length > 60 ? [...blocks.slice(0, 20), ...blocks.slice(-20)] : blocks);
  const head = $derived(blocks[0]);
  const streaming = $derived(blocks.some((block) => block.state === 'streaming'));
  const active = $derived(blocks.findLast((block) => block.state === 'streaming'));
  const started = $derived(blocks.find((block) => block.state === 'streaming')?.startedAt ?? head?.createdAt ?? now);
  const phase = $derived(active?.kind === 'tool_call' || active?.kind === 'tool_result' ? 'tool' : active?.kind === 'reasoning' ? 'thinking' : 'working');
  const agent = $derived(head?.agent ? head.agent[0]!.toLocaleUpperCase('ru-RU') + head.agent.slice(1) : 'Агент');
</script>

<article class="card">
  {#if head}
    <header>
      <p><strong>{agent}</strong><time>{fmt.clock(head.createdAt)}</time></p>
      <IconButton icon="info" label="Сведения о ходе" onclick={() => onInfo?.(head.inputCommandId || head.id)} />
    </header>
  {/if}
  {#each shown as block (block.id)}
    {#if block.toolName === 'Agent'}
      <SubagentRow call={block} nodes={childBlocks(all, block.id)} {conversationId} />
    {:else if block.kind === 'tool_call' || block.kind === 'tool_result'}
      <ToolBlock {block} {now} {load} />
    {:else if block.kind === 'warning' || block.kind === 'error'}
      <NoticeBlock {block} />
    {:else if block.kind === 'reasoning'}
      <ReasoningBlock {block} />
    {:else}
      {#if block.state === 'incomplete'}<p class="incomplete">Неполный ответ</p>{/if}
      <TextBlock {block} />
      {#if block.state === 'incomplete' && block.incompleteReason}<p class="incomplete">{block.incompleteReason}</p>{/if}
    {/if}
  {/each}
  {#if !expanded && blocks.length > 60}
    <button type="button" class="more" onclick={() => { expanded = true; }}>Показать ещё {hidden} {fmt.plural(hidden, ['блок', 'блока', 'блоков'])}</button>
  {/if}
  {#if streaming && head}
    <LiveLine {phase} tool={active?.toolName ?? ''} startedAt={started} {now} />
  {/if}
</article>

<style>
  .card {
    background: var(--surface);
    border-radius: var(--radius-card);
    padding: var(--s3) var(--s6) var(--s6);
    display: flex;
    flex-direction: column;
    gap: var(--s2);
    min-width: 0;
  }
  header { display: flex; justify-content: space-between; align-items: center; gap: var(--s2); margin-right: calc(var(--s3) * -1); }
  header p { display: flex; align-items: baseline; gap: var(--s4); min-width: 0; margin: 0; }
  strong { font-weight: var(--weight-semibold); }
  time { color: var(--time); font-size: var(--fs-sm); font-variant-numeric: tabular-nums; }
  header :global(button) { color: var(--secondary); }
  .incomplete { margin: 0; color: var(--warning); font-size: var(--fs-sm); }
  .more { align-self: flex-start; min-height: var(--target); padding-inline: var(--s6); color: var(--accent-link); font-size: var(--fs-sm); }
</style>
