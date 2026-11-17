<script lang="ts">
  import type { Snippet } from 'svelte';
  import { blockFold } from '#lib/ui/fold.js';
  import Icon from '#lib/ui/Icon.svelte';
  import type { TimelineEntry } from '#lib/stores/timeline.js';
  import type { FeedFolds } from '#lib/timeline/folds.js';
  import type { QuietGroup } from '#lib/timeline/steps.js';
  import { isMainAgent } from '#lib/timeline/present.js';
  import { EXPLORE } from '#lib/timeline/steps.js';
  import { feedCards } from './context';
  import { exploreCounts, exploreLine } from './defs/explore';
  import { quietName } from './defs/tools';
  import { toolId } from './toolId';

  let { group, folds, row }: { group: QuietGroup; folds: FeedFolds; row: Snippet<[TimelineEntry]> } = $props();
  const feed = feedCards();
  const open = $derived(folds.isOpen(group.key));
  // Имена по одному разу, в порядке вызовов: «Поиск инструментов, Наблюдение».
  const names = $derived([...new Set(group.entries.flatMap((entry) => (entry.kind === 'block' ? [quietName(toolId(entry.block.toolName))] : [])))]);
  const explore = $derived.by(() => {
    if (group.family !== EXPLORE) return null;
    const blocks = group.entries.flatMap((entry) => (entry.kind === 'block' ? [entry.block] : []));
    const agent = blocks[0]?.agent ?? '';
    return { ...exploreCounts(blocks), voice: feed.root && isMainAgent(agent) ? ('she' as const) : ('neutral' as const) };
  });
  const line = $derived(
    explore
      ? exploreLine(explore.files, explore.searches, { running: group.running, voice: explore.voice })
      : `Служебные вызовы · ${group.entries.length} · ${names.join(', ')}`
  );
  const status = $derived(group.running ? 'идёт' : group.failed > 0 ? `ошибка ${group.failed}` : '');
</script>

<div class="quiet" data-role="quiet-group" data-family={group.family}>
  <button type="button" class="head" aria-expanded={open} onclick={() => folds.toggle(group.key)}>
    <span class="chevron" data-open={open ? '' : undefined}><Icon name="chevron" /></span>
    <span class="line">{line}</span>
    {#if status}<span class="word" data-role="quiet-state" data-tone={group.running ? 'accent' : 'danger'}>{status}</span>{/if}
  </button>
  {#if open}
    <div class="fold" transition:blockFold>
      <div class="rows">
        {#each group.entries as entry (entry.key)}
          <div data-key={entry.key}>{@render row(entry)}</div>
        {/each}
      </div>
    </div>
  {/if}
</div>

<style>
  .quiet { display: flex; flex-direction: column; min-width: 0; }
  .head { display: flex; align-items: center; gap: var(--s3); width: 100%; min-height: var(--target); padding: var(--s2) var(--s5) var(--s2) var(--s4); border-radius: var(--radius-card); background: var(--transparent); font-size: var(--fs-sm); text-align: left; }
  .chevron { flex: none; display: inline-flex; color: var(--muted); transition: transform var(--duration-expand) var(--ease-expand); }
  .chevron :global(svg) { width: var(--icon-small); height: var(--icon-small); }
  .chevron[data-open] { transform: rotate(90deg); }
  .line { min-width: 0; overflow: hidden; color: var(--muted); text-overflow: ellipsis; white-space: nowrap; }
  .quiet[data-family='explore'] .line { color: var(--secondary); }
  .word { flex: none; margin-inline-start: auto; white-space: nowrap; }
  .word[data-tone='accent'] { color: var(--accent-link); }
  .word[data-tone='danger'] { color: var(--danger); }
  .fold { display: grid; grid-template-rows: 1fr; }
  .rows { display: flex; flex-direction: column; gap: var(--s1); min-height: 0; min-width: 0; overflow: hidden; }
</style>
