<script lang="ts">
  import { resolve } from '$app/paths';
  import { fmt } from '#lib/format/ru.js';
  import type { ConversationBookmarks } from '#lib/bookmarks/store.svelte.js';
  import type { InputView } from '#lib/stores/inputs.js';
  import { sessionMessageView } from '#lib/timeline/system.js';
  import IconButton from '#lib/ui/IconButton.svelte';
  import { timeTip } from '#lib/ui/time-tip.js';

  let {
    input,
    bookmarks = null
  }: {
    input: InputView;
    bookmarks?: ConversationBookmarks | null;
  } = $props();

  const view = $derived(sessionMessageView(input.origin, input.text));
  let expanded = $state(false);
  const bookmarked = $derived(input.id !== null && bookmarks?.has('input', input.id) === true);
</script>

{#if view}
  <!-- Сообщение другой сессии — на стороне агента: тише пузыря владельца и без его действий. -->
  <article class="message" data-origin={input.origin?.kind} data-role="session-message">
    <header>
      {#if view.href}
        <a class="title" href={resolve(view.href as '/')}>{view.title}</a>
      {:else}
        <span class="title">{view.title}</span>
      {/if}
      {#if view.model}<span class="model">{view.model}</span>{/if}
      {#if input.createdAt !== null}
        <time datetime={new Date(input.createdAt).toISOString()} use:timeTip={input.createdAt}>{fmt.clock(input.createdAt)}</time>
      {/if}
      {#if input.id && bookmarks}
        <div class="actions" data-role="bubble-actions">
          <IconButton
            icon="bookmark"
            solid={bookmarked}
            pressed={bookmarked}
            active={bookmarked}
            label={bookmarked ? 'Убрать из избранного' : 'В избранное'}
            onclick={() => bookmarks && void bookmarks.toggle('input', input.id!)}
          />
        </div>
      {/if}
    </header>
    <p class="text">{view.folded && !expanded ? view.folded : view.body}</p>
    {#if view.folded}
      <button type="button" class="toggle" aria-expanded={expanded} onclick={() => { expanded = !expanded; }}>
        {expanded ? 'Свернуть' : 'Показать полностью'}
      </button>
    {/if}
  </article>
{/if}

<style>
  .message {
    display: flex;
    flex-direction: column;
    gap: var(--s3);
    min-width: 0;
    max-width: var(--bubble-max);
    padding: var(--s5) var(--s6);
    border-radius: var(--radius-card);
    /* Тише пузыря владельца: чужое сообщение не спорит с собственным. */
    background: var(--raised);
  }
  header {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: var(--s2) var(--s4);
    min-width: 0;
  }
  .title {
    min-width: 0;
    color: var(--text);
    font-weight: var(--weight-medium);
    overflow-wrap: anywhere;
  }
  .title[href] { display: inline-flex; align-items: center; min-height: var(--target); margin-block: calc((var(--target) - var(--fs-body) * var(--lh-body)) / -2); color: var(--accent-link); text-decoration: none; }
  .title[href]:hover, .title[href]:focus-visible { text-decoration: underline; text-underline-offset: var(--s1); }
  .model { color: var(--secondary); font-size: var(--fs-sm); }
  time { margin-left: auto; color: var(--time); font-size: var(--fs-sm); font-variant-numeric: tabular-nums; }
  .text { margin: 0; color: var(--text); overflow-wrap: anywhere; white-space: pre-wrap; }
  .toggle { align-self: flex-start; min-height: var(--target); padding-inline: var(--s2); color: var(--accent-link); font-size: var(--fs-sm); }
  /* Закладка стоит в строке заголовка: цель касания та же, высота строки не растёт. */
  .actions { display: flex; align-self: center; margin-left: var(--s2); margin-block: calc((var(--target) - var(--fs-sm) * var(--lh-body)) / -2); }
</style>
