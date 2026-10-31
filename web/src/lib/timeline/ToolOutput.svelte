<script lang="ts">
  import { fmt } from '#lib/format/ru.js';
  import { stripAnsi } from '#lib/render/ansi.js';
  import type { BlockModel, BlockPage } from '#lib/stores/blocks.js';
  import { takeToolPage } from '#lib/timeline/present.js';

  let {
    block,
    running = false,
    load
  }: {
    block: BlockModel;
    running?: boolean;
    load: (blockId: string, offset: number) => Promise<BlockPage>;
  } = $props();
  let full = $state('');
  let capped = $state(false);
  let offset = $state(0);
  let busy = $state(false);
  const clean = $derived(stripAnsi(block.text));
  const tail = $derived(clean.split('\n').slice(-20).join('\n'));
  const lines = $derived(block.lineCount ?? 0);

  async function more(): Promise<void> {
    if (busy || capped) return;
    busy = true;
    try {
      const page = await load(block.id, offset);
      const next = takeToolPage(full, page.content);
      full = next.text;
      capped = next.capped;
      offset = page.next_offset ?? offset + page.content.length;
      if (page.next_offset == null) offset = page.total_bytes;
    } finally {
      busy = false;
    }
  }

  function save(): void {
    const blob = new Blob([full], { type: 'text/plain' });
    const url = URL.createObjectURL(blob);
    const link = document.createElement('a');
    link.href = url;
    link.download = 'tool-output.txt';
    link.click();
    URL.revokeObjectURL(url);
  }
</script>

<div class="out">
  {#if clean.trim() || full}
    <pre>{full || tail}</pre>
    <button type="button" onclick={more} aria-busy={busy}>Показать полностью{#if lines > 0}{` · ${lines} ${fmt.plural(lines, ['строка', 'строки', 'строк'])}`}{/if}</button>
    {#if full && offset < (block.contentBytes ?? Number.MAX_SAFE_INTEGER)}
      <button type="button" onclick={more}>Показать ещё</button>
    {/if}
    {#if capped}<button type="button" onclick={save}>Сохранить файлом</button>{/if}
  {:else}
    <p class="none">{running ? 'Вывод ещё не пришёл' : 'Вывода нет'}</p>
  {/if}
</div>

<style>
  .out { display: flex; flex-direction: column; align-items: flex-start; min-width: 0; padding: var(--s4) var(--s6) var(--s2); border-radius: var(--radius-card); background: var(--code); }
  pre { width: 100%; margin: 0; overflow-x: auto; max-width: 100%; border-radius: var(--radius-sm); white-space: pre; font-family: var(--font-mono); }
  .none { margin: var(--s0); padding-bottom: var(--s2); color: var(--secondary); font-size: var(--fs-sm); }
  button { min-height: var(--target); padding: var(--s0); border-radius: var(--radius-sm); background: var(--transparent); color: var(--accent-link); font-size: var(--fs-sm); text-align: left; }
</style>
