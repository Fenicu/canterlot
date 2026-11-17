<script lang="ts">
  import { fmt } from '#lib/format/ru.js';
  import { languageForPath } from '#lib/render/highlight.js';
  import CardShell from './CardShell.svelte';
  import CodeLines, { type CodeLine } from './CodeLines.svelte';
  import { changeVerb, fileDir, fileName, type EditData } from './defs/files';
  import { splitLines, type FileChange } from './diff';
  import type { CardProps } from './types';

  let { call, data, line, word, grows, quiet, env }: CardProps<EditData> = $props();
  /** Столько строк разницы видно в раскрытой карточке; остальное — по кнопке. */
  const SHOWN = 60;
  const key = $derived(`edit:${call.block.id}`);
  const all = $derived(env.folds.isOpen(key));
  const first = $derived(data.files[0]!);
  const verb = $derived(changeVerb(data.kind));

  interface Part {
    path: string;
    lang: string | null;
    lines: CodeLine[];
    signs: boolean;
    added: number;
    removed: number;
  }

  /** Новый файл показывается как код, без знаков; правка — разницей. */
  function part(file: FileChange): Part {
    const content = file.content !== undefined && file.kind !== 'edit';
    return {
      path: file.path,
      lang: languageForPath(file.path),
      lines: content ? splitLines(file.content ?? '').map((text) => ({ text })) : file.lines,
      signs: !content,
      added: file.added,
      removed: file.removed
    };
  }

  const parts = $derived.by<Part[]>(() => {
    if (data.shared.length > 0) return [{ path: '', lang: languageForPath(first.path), lines: data.shared, signs: true, added: data.added, removed: data.removed }];
    return data.files.filter((file) => file.lines.length > 0 || file.content).map(part);
  });
  const total = $derived(parts.reduce((sum, item) => sum + item.lines.length, 0));

  /** Части под предел строк: длинная разница режется, «Показать всё» снимает предел. */
  const shown = $derived.by(() => {
    let left = all ? Number.POSITIVE_INFINITY : SHOWN;
    return parts.map((item) => {
      const lines = item.lines.slice(0, Math.max(0, left));
      left -= lines.length;
      return { ...item, lines };
    });
  });
</script>

<!-- Свёрнутая карточка говорит, что случилось: какой файл, новый он или правленый, сколько строк. -->
<CardShell {call} {env} {line} {grows} {quiet} status={word?.word ?? ''} tone={word?.tone ?? 'muted'} reveal="изменения">
  {#snippet head()}
    <span class="verb" data-role="edit-kind" data-kind={data.kind}>{verb}</span>
    <span class="name" data-role="edit-name">{fileName(first.path)}</span>
    {#if data.files.length > 1}<span class="more-files">и ещё {data.files.length - 1}</span>{:else if fileDir(first.path)}<span class="dir" data-role="edit-dir">{fileDir(first.path)}</span>{/if}
    {#if data.counted}
      <span class="counts" data-role="edit-counts"><span class="added">+{data.added}</span>{#if data.removed > 0 || data.kind === 'edit'}<span class="removed">−{data.removed}</span>{/if}</span>
    {/if}
  {/snippet}
  {#snippet fold()}
    {#if data.files.length > 1 && (data.shared.length > 0 || parts.length === 0)}
      <!-- Codex прислал общую разницу без путей: файлы списком, строки одним блоком. -->
      <ul class="files">
        {#each data.files as file, index (index)}<li class="path" data-role="edit-file">{file.path}</li>{/each}
      </ul>
    {/if}
    {#each shown as item, index (index)}
      {#if item.lines.length > 0}
        {#if data.files.length > 1 && item.path}
          <p class="part" data-role="edit-file"><span class="path">{item.path}</span><span class="added">+{item.added}</span>{#if item.removed > 0}<span class="removed">−{item.removed}</span>{/if}</p>
        {/if}
        <CodeLines lines={item.lines} lang={item.lang} signs={item.signs} label={item.path ? `Изменения ${fileName(item.path)}` : 'Изменения'} />
      {/if}
    {/each}
    {#if total > SHOWN}
      <button type="button" class="more" aria-expanded={all} onclick={() => env.folds.toggle(key)}>{all ? 'Свернуть' : `Показать всё · ${total} ${fmt.plural(total, ['строка', 'строки', 'строк'])}`}</button>
    {/if}
    {#if parts.length === 0}
      <p class="none">{call.state === 'running' ? 'Изменения ещё не пришли' : 'Текста изменений нет'}</p>
    {/if}
  {/snippet}
</CardShell>

<style>
  .verb { flex: none; color: var(--secondary); }
  .name { flex: 0 1 auto; min-width: 0; overflow: hidden; color: var(--text); font-weight: var(--weight-medium); text-overflow: ellipsis; white-space: nowrap; }
  .dir, .more-files { flex: 1 1 auto; min-width: 0; overflow: hidden; color: var(--muted); text-overflow: ellipsis; white-space: nowrap; }
  .counts { display: inline-flex; flex: none; gap: var(--s3); margin-inline-start: auto; font-family: var(--font-mono); font-variant-numeric: tabular-nums; }
  .added { color: var(--success); }
  .removed { color: var(--danger); }
  .files { display: flex; flex-direction: column; gap: var(--s2); margin: var(--s0); padding: var(--s2) var(--s4); list-style: none; font-size: var(--fs-sm); }
  .path { min-width: 0; overflow: hidden; color: var(--secondary); font-family: var(--font-mono); text-overflow: ellipsis; white-space: nowrap; }
  .part { display: flex; align-items: baseline; gap: var(--s3); min-width: 0; margin: var(--s2) var(--s4) var(--s0); font-family: var(--font-mono); font-size: var(--fs-sm); }
  .part .added { margin-inline-start: auto; }
  .part .added, .part .removed { flex: none; }
  .more { align-self: flex-start; min-height: var(--target); padding: var(--s0) var(--s4); background: var(--transparent); color: var(--accent-link); font-size: var(--fs-sm); }
  .none { margin: var(--s0); padding: var(--s4) var(--s6); border-radius: var(--radius-card); background: var(--code); color: var(--secondary); font-size: var(--fs-sm); }
</style>
