<script lang="ts">
  import { untrack } from 'svelte';
  import { TODO_STATUS_LABEL } from '#lib/screens/chat/todo.js';
  import { arrivedNow } from '#lib/timeline/arrival.js';
  import CardShell from './CardShell.svelte';
  import type { TodoData, TodoEntry } from './defs/todo';
  import type { CardProps } from './types';

  let { call, data, line, env }: CardProps<TodoData> = $props();
  // Черта проводится только у пункта, закрытого на глазах: история показывает готовое сразу зачёркнутым.
  const fresh = untrack(() => arrivedNow(`todo:${call.block.id}`, call.block.createdAt));
  const key = $derived(`todo:${call.block.id}`);
  const all = $derived(env.folds.isOpen(key));
  // Свёрнутая карточка — только перемены и текущий пункт: план целиком лежит в панели над вводом.
  const changed = $derived.by(() => {
    const picked = data.items.filter((item) => item.done || item.added || item.status === 'in_progress');
    return picked.length > 0 ? picked : data.items.slice(0, 1);
  });
  const shown = $derived(all ? data.items : changed);
  const hidden = $derived(data.items.length - changed.length);
  const progress = $derived(`${data.done} из ${data.total}${data.cut ? '+' : ''}`);

  function note(item: TodoEntry): string {
    if (item.done) return 'готово';
    if (item.added && item.status === 'pending') return 'новый';
    return TODO_STATUS_LABEL[item.status];
  }

  function settle(node: HTMLElement) {
    if (!fresh) return;
    node.setAttribute('data-arriving', '');
    const frame = requestAnimationFrame(() => requestAnimationFrame(() => node.removeAttribute('data-arriving')));
    return { destroy: () => cancelAnimationFrame(frame) };
  }
</script>

<CardShell {call} {env} {line} status={progress} tone={data.done === data.total && data.total > 0 ? 'success' : 'muted'}>
  {#snippet head()}<span class="title">Задачи</span>{#if data.closed.length > 0}<span class="delta" data-role="todo-delta">{data.closed.length === 1 ? 'закрыт пункт' : `закрыто ${data.closed.length}`}</span>{:else if data.added.length > 0}<span class="delta" data-role="todo-delta">{`новых ${data.added.length}`}</span>{/if}{/snippet}
  <ol class="items" use:settle>
    {#each shown as item, index (index + item.text)}
      <li class="item" data-status={item.status} data-done={item.done ? '' : undefined} data-added={item.added ? '' : undefined}>
        <span class="mark" aria-hidden="true"></span>
        <span class="text">{item.text}{#if item.status === 'completed'}<span class="strike" aria-hidden="true">{item.text}</span>{/if}</span>
        <span class="word" data-status={item.done ? 'completed' : item.status}>{note(item)}</span>
      </li>
    {/each}
  </ol>
  {#if hidden > 0 || all}
    <button type="button" class="more" aria-expanded={all} onclick={() => env.folds.toggle(key)}>{all ? 'Только перемены' : `Весь список · ${data.items.length}`}</button>
  {/if}
</CardShell>

<style>
  .title { flex: none; color: var(--text); }
  .delta { min-width: 0; overflow: hidden; color: var(--secondary); text-overflow: ellipsis; white-space: nowrap; }
  .items { display: flex; flex-direction: column; gap: var(--s2); margin: var(--s0); padding: var(--s0); list-style: none; }
  .item { display: grid; grid-template-columns: var(--mark) minmax(var(--s0), 1fr) auto; align-items: baseline; gap: var(--s4); color: var(--text); transition: color var(--duration-todo-strike) var(--ease-todo-strike); }
  .item:is([data-status='completed'], [data-status='cancelled']) { color: var(--secondary); }
  .text { position: relative; overflow-wrap: anywhere; }
  /* Та же черта, что у панели над вводом: копия текста без букв, видна только линия. */
  .strike {
    position: absolute;
    inset: var(--s0);
    -webkit-text-fill-color: var(--transparent);
    text-decoration-line: line-through;
    text-decoration-color: var(--secondary);
    text-decoration-thickness: var(--hairline);
    clip-path: inset(var(--s0));
    pointer-events: none;
    user-select: none;
    transition: clip-path var(--duration-todo-strike) var(--ease-todo-strike);
  }
  .items:global([data-arriving]) .item[data-done] .strike { clip-path: inset(var(--s0) 100% var(--s0) var(--s0)); transition: none; }
  .items:global([data-arriving]) .item[data-done] { color: var(--text); transition: none; }
  .items:global([data-arriving]) .item[data-done] .mark::after { background: var(--accent); transition: none; }
  .mark { display: flex; align-items: center; }
  .mark::before { content: '\200b'; }
  .mark::after { content: ''; flex: none; width: var(--mark); height: var(--mark); border-radius: var(--radius-round); background: var(--badge); transition: background-color var(--duration-todo-strike) var(--ease-todo-strike); }
  .item[data-status='in_progress'] .mark::after { background: var(--accent); }
  .item[data-status='completed'] .mark::after { background: var(--success); }
  .word { flex: none; color: var(--muted); white-space: nowrap; transition: color var(--duration-todo-strike) var(--ease-todo-strike); }
  .word[data-status='in_progress'] { color: var(--accent-link); }
  .word[data-status='completed'] { color: var(--success); }
  .word[data-status='cancelled'] { color: var(--warning); }
  .item[data-added] .word[data-status='pending'] { color: var(--accent-link); }
  .more { align-self: flex-start; min-height: var(--target); margin-block: calc(var(--s2) * -1); padding: var(--s0); background: var(--transparent); color: var(--accent-link); }
</style>
