<script lang="ts">
  import type { ConversationBookmarks } from '#lib/bookmarks/store.svelte.js';
  import type { BlockModel } from '#lib/stores/blocks.js';
  import SystemBubble from '#lib/timeline/SystemBubble.svelte';
  import { apiErrorView } from '#lib/timeline/system.js';

  let {
    block,
    harness = null,
    bookmarks = null,
    open,
    onToggle
  }: {
    block: BlockModel;
    harness?: { id?: string; title?: string; provider?: string } | null;
    /** Закладки разговора: отметка стоит на самом блоке ошибки. */
    bookmarks?: ConversationBookmarks | null;
    open: boolean;
    onToggle: () => void;
  } = $props();

  const view = $derived(apiErrorView(block, harness));
  const marked = $derived(bookmarks?.has('block', block.id) === true);
</script>

{#if view}
  <!-- Ошибка API провайдера отдельным пузырём, на месте блока в ленте. -->
  <SystemBubble
    {view}
    createdAt={block.createdAt > 0 ? block.createdAt : null}
    {open}
    {onToggle}
    bookmark={bookmarks
      ? {
          marked,
          label: marked ? 'Убрать из избранного' : 'В избранное',
          onToggle: () => {
            if (bookmarks) void bookmarks.toggle('block', block.id);
          }
        }
      : null}
  />
{/if}
