<script lang="ts">
  import { onMount } from 'svelte';
  import { goto } from '$app/navigation';
  import { resolve } from '$app/paths';
  import { clock } from '#lib/clock.js';
  import { ticker } from '#lib/ticker.svelte.js';
  import { theme } from '#lib/theme.js';
  import TopBar from '#lib/ui/TopBar.svelte';
  import Icon from '#lib/ui/Icon.svelte';
  import IconButton from '#lib/ui/IconButton.svelte';
  import BookmarkRow from './BookmarkRow.svelte';
  import { bookmarkHref, type BookmarkView } from './model.js';
  import { BookmarksListStore, bookmarksPageApi } from './store.svelte.js';

  const store = new BookmarksListStore(bookmarksPageApi);
  let scroller = $state<HTMLElement | null>(null);
  let sentinel = $state<HTMLElement | null>(null);
  const now = $derived.by(() => {
    void ticker.second;
    return clock.serverNow();
  });

  onMount(() => {
    void store.run();
    return () => store.dispose();
  });

  function pick(entry: BookmarkView): void {
    // Тот же адрес и тот же прыжок, что у совпадения поиска.
    // eslint-disable-next-line svelte/no-navigation-without-resolve
    void goto(bookmarkHref(entry));
  }

  // Подгрузка следующей страницы по курсору, когда низ списка показался на экране.
  $effect(() => {
    const root = scroller;
    const node = sentinel;
    void store.cursor;
    if (!root || !node) return;
    const observer = new IntersectionObserver(
      (entries) => {
        if (entries.some((entry) => entry.isIntersecting)) void store.more();
      },
      { root, rootMargin: '0px 0px 240px 0px' }
    );
    observer.observe(node);
    return () => observer.disconnect();
  });
</script>

<main
  id="main"
  tabindex="-1"
  class="bookmarks-screen"
  data-bookmarks-screen
  data-status={store.status}
  data-ready={store.status === 'ready' || store.status === 'error' ? '' : undefined}
>
  <TopBar title="Избранное">
    {#snippet leading()}
      <a class="back" href={resolve('/sessions')} aria-label="К сессиям"><Icon name="back" /></a>
    {/snippet}
    {#snippet actions()}<IconButton icon={theme.effective === 'dark' ? 'sun' : 'moon'} label="Сменить тему" onclick={() => theme.toggle()} />{/snippet}
  </TopBar>

  <div class="results" bind:this={scroller} data-bookmarks-results>
    {#if store.status === 'loading' && store.items.length === 0}
      <p class="state" role="status" data-bookmarks-loading>Загружаю избранное…</p>
    {:else if store.status === 'error' && store.failure}
      <div class="state" role="alert" data-bookmarks-error>
        <p>{store.failure}</p>
        <button type="button" class="retry" onclick={() => void store.run()}>Повторить</button>
      </div>
    {:else if store.status === 'ready' && store.items.length === 0}
      <p class="state" data-bookmarks-empty>Здесь появятся сообщения, отмеченные закладкой в чате.</p>
    {:else}
      {#if store.actionError}
        <p class="error" role="alert">{store.actionError}</p>
      {/if}
      <ul class="rows">
        {#each store.items as entry (entry.id)}
          <BookmarkRow
            {entry}
            {now}
            showConversation
            onopen={pick}
            onremove={(item) => void store.remove(item)}
          />
        {/each}
      </ul>
      {#if store.moreFailure}
        <div class="state" role="alert">
          <p>{store.moreFailure}</p>
          <button type="button" class="retry" onclick={() => void store.more()}>Повторить</button>
        </div>
      {:else if store.loadingMore}
        <p class="state" role="status">Загружаю ещё…</p>
      {/if}
      <div class="sentinel" bind:this={sentinel} aria-hidden="true"></div>
    {/if}
  </div>
</main>

<style>
  .bookmarks-screen {
    display: flex;
    flex: 1;
    flex-direction: column;
    min-width: 0;
    min-height: 0;
    width: 100%;
    max-width: var(--reading-max);
    margin-inline: auto;
  }
  .back {
    display: flex;
    align-items: center;
    justify-content: center;
    min-width: var(--target);
    min-height: var(--target);
    padding: var(--s4);
    color: var(--text);
  }
  .results {
    flex: 1;
    min-height: 0;
    overflow-x: hidden;
    overflow-y: auto;
    padding: 0 var(--gutter) var(--s10);
  }
  .rows { display: flex; flex-direction: column; margin: 0; padding: 0; list-style: none; }
  .state { margin: var(--s4) var(--s2); color: var(--secondary); }
  .error { margin: var(--s2) var(--s2) var(--s4); color: var(--danger); font-size: var(--fs-sm); }
  .retry { min-height: var(--target); padding-inline: var(--s6); border: var(--s0); border-radius: var(--radius-pill); background: var(--transparent); color: var(--accent-link); font-size: var(--fs-meta); }
  .sentinel { height: var(--s1); }
</style>
