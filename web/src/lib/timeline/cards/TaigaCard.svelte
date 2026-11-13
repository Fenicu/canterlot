<script lang="ts">
  import CardLine from './CardLine.svelte';
  import CardShell from './CardShell.svelte';
  import type { TaigaData, TaigaRow } from './defs/tools';
  import type { CardProps } from './types';

  let { call, data, line, link, word, grows, env }: CardProps<TaigaData> = $props();

  const moreKey = $derived(`taiga-more:${call.block.id}`);
  const all = $derived(env.folds.isOpen(moreKey));
  const commentKey = $derived(`taiga-comment:${call.block.id}`);
  const commentOpen = $derived(env.folds.isOpen(commentKey));
  const COMMENT_LONG = 300;
  const commentLong = $derived(data.comment.length > COMMENT_LONG || data.comment.split('\n').length > 6);
  const shown = (rows: TaigaRow[]) => (all ? rows : rows.slice(0, 5));
  const KIND_WORD: Record<string, string> = { userstory: 'история', task: 'задача', issue: 'запрос', epic: 'эпик', wiki: 'вики' };
  const SEP = ' · ';
  const SPACE = ' ';

  const parts = $derived.by(() => {
    const out: { text: string; danger: boolean }[] = [];
    if (data.status) out.push({ text: data.status, danger: false });
    if (data.assigned) out.push({ text: data.assigned, danger: false });
    if (data.sprint) out.push({ text: data.sprint, danger: false });
    if (data.points) out.push({ text: data.points, danger: false });
    if (data.blocked) out.push({ text: data.blockedWord, danger: true });
    return out;
  });
</script>

<CardShell {call} {env} {line} {grows} status={word?.word ?? ''} tone={word?.tone ?? 'muted'} live={word?.live}>
  {#snippet head()}<CardLine {line} {link} />{/snippet}

  {#if data.error}
    <p class="danger" data-role="taiga-error">{data.error}</p>
  {/if}

  {#if data.kind === 'get' || data.kind === 'create'}
    {#if parts.length > 0}
      <p class="meta" data-role="taiga-meta">
        {#each parts as part, index (index)}
          {#if index > 0}<span class="sep">{SEP}</span>{/if}<span class="part" class:blocked={part.danger} data-role={part.danger ? 'taiga-blocked' : undefined}>{part.text}</span>
        {/each}
      </p>
    {/if}
  {/if}

  {#if data.kind === 'update'}
    {#if data.changes.length > 0}
      <ul class="changes" data-role="taiga-changes">
        {#each data.changes as change, index (index)}<li>{change}</li>{/each}
      </ul>
    {/if}
    {#if data.hint}<p class="meta" data-role="taiga-hint">{data.hint}</p>{/if}
  {/if}

  {#if data.kind === 'bulk'}
    {#if data.created.length > 0}
      <ul class="rows" data-role="taiga-created">
        {#each shown(data.created) as row, index (index)}<li class="row">{#if row.ref}<span class="ref">{`#${row.ref}`}</span>{SPACE}{/if}<span class="subject">{row.subject}</span></li>{/each}
      </ul>
      {#if data.created.length > 5}
        <button type="button" class="more" aria-expanded={all} data-role="taiga-more" onclick={() => env.folds.toggle(moreKey)}>{all ? 'Свернуть' : `ещё ${data.created.length - 5}`}</button>
      {/if}
    {/if}
    {#if data.failed > 0}<p class="meta danger" data-role="taiga-failed">не создано: {data.failed}</p>{/if}
  {/if}

  {#if data.kind === 'comment_add'}
    {#if data.comment}
      <div class="quote" class:clamped={!commentOpen && commentLong} data-role="taiga-comment">{data.comment}</div>
      {#if commentLong}
        <button type="button" class="more" aria-expanded={commentOpen} data-role="taiga-comment-toggle" onclick={() => env.folds.toggle(commentKey)}>{commentOpen ? 'Свернуть' : 'Развернуть'}</button>
      {/if}
    {/if}
  {/if}

  {#if data.kind === 'comment_list'}
    {#if data.lastComment}
      <div class="quote" data-role="taiga-last-comment">{#if data.lastComment.author}<span class="author">{data.lastComment.author}</span><span class="sep">{SEP}</span>{/if}{data.lastComment.text}</div>
    {/if}
  {/if}

  {#if data.kind === 'list' || data.kind === 'search'}
    {#if data.rows.length > 0}
      <ul class="rows" data-role="taiga-rows">
        {#each shown(data.rows) as row, index (index)}
          <li class="row">
            {#if data.kind === 'search' && row.kind}<span class="kind">{KIND_WORD[row.kind] ?? row.kind}</span>{SPACE}{/if}
            {#if row.ref}<span class="ref">{`#${row.ref}`}</span>{SPACE}{/if}
            <span class="subject">{row.subject}</span>
            {#if row.status}{SEP}<span class="status">{row.status}</span>{/if}
          </li>
        {/each}
      </ul>
      {#if data.rows.length > 5}
        <button type="button" class="more" aria-expanded={all} data-role="taiga-more" onclick={() => env.folds.toggle(moreKey)}>{all ? 'Свернуть' : `ещё ${data.rows.length - 5}`}</button>
      {/if}
    {/if}
  {/if}
</CardShell>

<style>
  p { margin: var(--s0); }
  .meta { color: var(--secondary); overflow-wrap: anywhere; }
  .sep { color: var(--muted); }
  .part { color: var(--secondary); }
  .blocked { color: var(--danger); }
  .danger { color: var(--danger); }
  .changes { display: flex; flex-direction: column; gap: var(--s2); margin: var(--s0); padding: var(--s0); color: var(--secondary); list-style: none; }
  .changes li { overflow-wrap: anywhere; }
  .rows { display: flex; flex-direction: column; gap: var(--s2); margin: var(--s0); padding: var(--s0); list-style: none; }
  .row { min-width: 0; overflow: hidden; color: var(--text); text-overflow: ellipsis; white-space: nowrap; }
  .kind { color: var(--muted); }
  .ref { color: var(--secondary); font-variant-numeric: tabular-nums; }
  .status { color: var(--muted); }
  .quote { padding: var(--s4) var(--s6); border-radius: var(--radius-sm); background: var(--surface); color: var(--secondary); overflow-wrap: anywhere; white-space: pre-wrap; }
  .quote.clamped { display: -webkit-box; overflow: hidden; -webkit-box-orient: vertical; -webkit-line-clamp: 6; line-clamp: 6; }
  .author { color: var(--muted); }
  .more { align-self: flex-start; min-width: var(--target); min-height: var(--target); margin-block: calc(var(--s2) * -1); padding: var(--s0); background: var(--transparent); color: var(--accent-link); }
</style>
