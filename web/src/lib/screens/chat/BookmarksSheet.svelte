<script lang="ts">
  import Sheet from '#lib/ui/Sheet.svelte';
  import BookmarkRow from '#lib/bookmarks/BookmarkRow.svelte';
  import type { ConversationBookmarks } from '#lib/bookmarks/store.svelte.js';
  import type { BookmarkView } from '#lib/bookmarks/model.js';

  let {
    marks,
    now,
    onclose,
    onpick
  }: {
    marks: ConversationBookmarks;
    now: number;
    onclose: () => void;
    /** Переход к сообщению тем же прыжком, что у совпадения поиска. */
    onpick: (entry: BookmarkView) => void;
  } = $props();
</script>

<Sheet open title="Избранное" {onclose}>
  {#if marks.loading && !marks.loaded}
    <p class="state" role="status">Загружаю избранное…</p>
  {:else if marks.loadError}
    <div class="state" role="alert">
      <p>{marks.loadError}</p>
      <button type="button" class="retry" onclick={() => void marks.load()}>Повторить</button>
    </div>
  {:else if marks.list.length === 0}
    <p class="state" data-bookmarks-empty>В этой сессии пока ничего не отмечено. Закладка ставится кнопкой под ответом или в действиях своего сообщения.</p>
  {:else}
    <ul class="rows">
      {#each marks.list as entry (entry.id)}
        <BookmarkRow {entry} {now} onopen={onpick} onremove={(item) => void marks.remove(item.target_kind, item.target_id)} />
      {/each}
    </ul>
  {/if}
</Sheet>

<style>
  .state { margin: var(--s4) var(--s2); color: var(--secondary); }
  .rows { display: flex; flex-direction: column; margin: 0; padding: 0; list-style: none; }
  .retry { min-height: var(--target); padding-inline: var(--s6); border: var(--s0); border-radius: var(--radius-pill); background: var(--transparent); color: var(--accent-link); font-size: var(--fs-meta); }
</style>
