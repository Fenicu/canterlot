<script lang="ts">
  import { fmt } from '$lib/format/ru';
  import type { BlockPage } from '$lib/stores/blocks';
  import type { TimelineEntry } from '$lib/stores/timeline';
  import { childBlocks } from '$lib/timeline/rows';
  import LiveLine from '$lib/timeline/LiveLine.svelte';
  import NoticeBlock from '$lib/timeline/NoticeBlock.svelte';
  import SubagentRow from '$lib/timeline/SubagentRow.svelte';
  import TextBlock from '$lib/timeline/TextBlock.svelte';
  import ToolBlock from '$lib/timeline/ToolBlock.svelte';

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
  const started = $derived(blocks.find((block) => block.state === 'streaming')?.startedAt ?? head?.createdAt ?? now);
</script>

<article class="card">
  {#if head}
    <header>
      <p>{head.agent || 'агент'} · {fmt.clock(head.createdAt)} · {head.model}</p>
      <button type="button" aria-label="Сведения о ходе" onclick={() => onInfo?.(head.inputCommandId || head.id)}>i</button>
    </header>
  {/if}
  {#each shown as block (block.id)}
    {#if block.toolName === 'Agent'}
      <SubagentRow call={block} nodes={childBlocks(all, block.id)} {conversationId} />
    {:else if block.kind === 'tool_call' || block.kind === 'tool_result'}
      <ToolBlock {block} {now} {load} />
    {:else if block.kind === 'warning' || block.kind === 'error'}
      <NoticeBlock {block} />
    {:else}
      {#if block.state === 'incomplete'}<p>Неполный ответ</p>{/if}
      <TextBlock {block} />
      {#if block.state === 'incomplete' && block.incompleteReason}<p>{block.incompleteReason}</p>{/if}
    {/if}
  {/each}
  {#if !expanded && blocks.length > 60}
    <button type="button" onclick={() => { expanded = true; }}>Показать ещё {hidden} блоков</button>
  {/if}
  {#if streaming && head}
    <LiveLine phase="working" startedAt={started} {now} />
  {/if}
</article>

<style>
  .card {
    background: var(--surface);
    border-radius: var(--radius-card);
    padding: var(--s3);
    display: flex;
    flex-direction: column;
    gap: var(--s2);
    min-width: 0;
  }
  header { display: flex; justify-content: space-between; gap: var(--s2); }
  header p, p { margin: 0; }
</style>
