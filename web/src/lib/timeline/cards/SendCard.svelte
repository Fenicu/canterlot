<script lang="ts">
  import CardLine from './CardLine.svelte';
  import CardShell from './CardShell.svelte';
  import { say } from './copy';
  import type { SendData } from './defs/sessions';
  import type { CardProps } from './types';

  let { call, data, line, link, word, env }: CardProps<SendData> = $props();
  const key = $derived(`text:${call.block.id}`);
  const open = $derived(env.folds.isOpen(key));
  // Два ряда текста видны сразу; длинное сообщение раскрывается целиком.
  const long = $derived(data.text.length > 140 || data.text.split('\n').length > 2 || data.textCut);
  const status = $derived(word?.word ?? (data.subscribed ? say(call.voice, 'подписана', 'с подпиской') : ''));
  const warning = $derived(data.stopped ? 'Получатель остановлен: сообщение ждёт в очереди' : data.paused ? 'Очередь получателя на паузе у владельца' : '');
</script>

<CardShell {call} {env} {line} {status} tone={word?.tone ?? 'muted'}>
  {#snippet head()}<CardLine {line} {link} />{/snippet}
  {#if data.text}
    <p class="text" data-open={open ? '' : undefined} data-role="send-text">{data.text}{#if data.textCut && open}…{/if}</p>
    {#if long}
      <button type="button" class="more" aria-expanded={open} onclick={() => env.folds.toggle(key)}>{open ? 'Свернуть' : 'Весь текст'}</button>
    {/if}
  {/if}
  {#if warning}<p class="warn">{warning}</p>{/if}
  {#if data.error}<p class="danger">{data.error}</p>{/if}
</CardShell>

<style>
  p { margin: var(--s0); overflow-wrap: anywhere; }
  .text { display: -webkit-box; overflow: hidden; color: var(--secondary); white-space: pre-wrap; -webkit-box-orient: vertical; -webkit-line-clamp: 2; line-clamp: 2; }
  .text[data-open] { display: block; -webkit-line-clamp: unset; line-clamp: none; }
  .more { align-self: flex-start; min-height: var(--target); margin-block: calc(var(--s2) * -1); padding: var(--s0); background: var(--transparent); color: var(--accent-link); }
  .warn { color: var(--warning); }
  .danger { color: var(--danger); }
</style>
