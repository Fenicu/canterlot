<script lang="ts">
  import IconButton from '#lib/ui/IconButton.svelte';
  import { timeTip } from '#lib/ui/time-tip.js';
  import { bookmarkExcerpt, bookmarkStamp, bookmarkWho, type BookmarkView } from './model.js';

  let {
    entry,
    now,
    showConversation = false,
    onopen,
    onremove
  }: {
    entry: BookmarkView;
    now: number;
    /** В общем списке первая строка — название разговора. */
    showConversation?: boolean;
    onopen: (entry: BookmarkView) => void;
    onremove: (entry: BookmarkView) => void;
  } = $props();

  const who = $derived(bookmarkWho(entry));
  const stamp = $derived(bookmarkStamp(entry, now));
  const excerpt = $derived(bookmarkExcerpt(entry));
  const at = $derived(Date.parse(entry.target_created_at));
  const empty = $derived(entry.excerpt.trim().length === 0);
</script>

<li class="row" data-bookmark-row data-bookmark-id={entry.id}>
  <button
    type="button"
    class="open"
    aria-label={`Открыть сообщение: ${who}, ${stamp}`}
    onclick={() => onopen(entry)}
  >
    {#if showConversation}
      <span class="conversation" data-bookmark-conversation>{entry.conversation_title || 'Сессия'}</span>
    {/if}
    <span class="head">
      <span class="who">{who}</span>
      {#if entry.archived}<span class="archived" data-bookmark-archived>в архиве</span>{/if}
      <time datetime={entry.target_created_at} use:timeTip={at}>{stamp}</time>
    </span>
    <span class="excerpt" class:empty data-bookmark-excerpt>{excerpt}</span>
  </button>
  <IconButton icon="bookmark" solid label="Убрать из избранного" onclick={() => onremove(entry)} />
</li>

<style>
  .row {
    display: flex;
    align-items: center;
    gap: var(--s2);
    min-width: 0;
  }
  .open {
    flex: 1;
    min-width: 0;
    min-height: var(--target);
    display: flex;
    flex-direction: column;
    gap: var(--s1);
    padding: var(--s4) var(--s2);
    border: var(--s0);
    border-radius: var(--radius-sm);
    background: var(--transparent);
    color: var(--text);
    text-align: start;
  }
  .open:focus-visible { outline: var(--focus-width) solid var(--accent); outline-offset: calc(var(--focus-width) * -1); }
  @media (hover: hover) and (pointer: fine) { .open:hover { background: var(--hover); } }
  .conversation, .who { font-weight: var(--weight-medium); }
  .conversation, .head, .excerpt { min-width: 0; }
  .conversation { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .head { display: flex; flex-wrap: wrap; align-items: baseline; gap: var(--s1) var(--s4); color: var(--secondary); font-size: var(--fs-sm); }
  .archived { color: var(--muted); }
  time { color: var(--time); font-variant-numeric: tabular-nums; }
  /* Длинный фрагмент не расширяет лист: две строки и многоточие. */
  .excerpt {
    display: -webkit-box;
    -webkit-box-orient: vertical;
    line-clamp: 2;
    -webkit-line-clamp: 2;
    overflow: hidden;
    overflow-wrap: anywhere;
  }
  .excerpt.empty { color: var(--muted); }
</style>
