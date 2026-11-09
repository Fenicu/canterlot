<script lang="ts">
  import { untrack } from 'svelte';
  import type { BackgroundFact } from '#lib/stores/background.svelte.js';
  import type { BlockModel, BlockPage } from '#lib/stores/blocks.js';
  import { arrivedNow } from '#lib/timeline/arrival.js';
  import type { FeedFolds } from '#lib/timeline/folds.js';
  import type { BlockLinks } from '#lib/timeline/rows.js';
  import ToolBlock from '#lib/timeline/ToolBlock.svelte';
  import { cardCall } from './call';
  import { feedCards } from './context';
  import LineCard from './LineCard.svelte';
  import { resolveCard } from './registry';
  import type { CardEnv } from './types';

  let {
    block,
    result = null,
    background = null,
    working = false,
    now,
    open,
    onToggle,
    load,
    links,
    folds,
    conversationId
  }: {
    block: BlockModel;
    result?: BlockModel | null;
    background?: BackgroundFact | null;
    working?: boolean;
    now: number;
    open: boolean;
    onToggle: () => void;
    load: (blockId: string, offset: number) => Promise<BlockPage>;
    links: BlockLinks;
    folds: FeedFolds;
    conversationId: string;
  } = $props();

  const feed = feedCards();
  const call = $derived(
    cardCall(block, result, {
      working,
      background,
      root: feed.root,
      harness: feed.harness,
      conversationId,
      session: feed.session,
      events: feed.events,
      previous: () => feed.previous(block),
      planBefore: (number) => feed.planBefore?.(block, number) ?? null,
      carrier: links.carrier(block.id)
    })
  );
  const card = $derived(block.kind === 'tool_call' ? resolveCard(call, now) : null);
  // Растёт только карточка, пришедшая на глазах; прочитанная история стоит сразу.
  const grows = untrack(() => (card?.def.grows ? arrivedNow(`card:${block.id}`, block.createdAt) : false));
  const env: CardEnv = $derived({ open, onToggle, load, now, working, links, background, conversationId, folds });
</script>

{#if card}
  {@const Card = card.def.component ?? LineCard}
  <!-- Сбой разметки карточки не оставляет пустого места: на его месте обычный блок инструмента. -->
  <svelte:boundary>
    <Card {call} data={card.data as never} line={card.line} link={card.link} word={card.word} {grows} quiet={card.def.quiet === true} {env} />
    {#snippet failed()}
      <ToolBlock {block} {result} {background} {working} {now} {load} {open} {onToggle} />
    {/snippet}
  </svelte:boundary>
{:else}
  <ToolBlock {block} {result} {background} {working} {now} {load} {open} {onToggle} />
{/if}
