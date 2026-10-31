<script lang="ts">
  import type { Snippet } from 'svelte';
  import { fmt } from '#lib/format/ru.js';
  import type { TimelineEntry } from '#lib/stores/timeline.js';
  import type { FeedFolds } from '#lib/timeline/folds.js';
  import StepHead from '#lib/timeline/StepHead.svelte';
  import type { StepGroup } from '#lib/timeline/steps.js';

  let { group, folds, row }: { group: StepGroup; folds: FeedFolds; row: Snippet<[TimelineEntry]> } = $props();
  const open = $derived(folds.isOpen(group.key));
  const view = $derived(open ? folds.view(group.key, group.entries, false) : null);
</script>

<div class="steps">
  <div class="surface">
    <StepHead
      name={group.label}
      detail={group.tally}
      status={group.failed > 0 ? `Ошибка ${group.failed}` : group.elapsed >= 1000 ? fmt.durationCoarse(group.elapsed) : ''}
      state={group.failed > 0 ? 'error' : 'done'}
      {open}
      ontoggle={() => folds.toggle(group.key)}
    />
  </div>
  {#if view}
    <div class="fold">
      <div class="rows">
        {#each view.shown as entry (entry.key)}
          {#if entry.key === view.seam}
            <button type="button" class="more" onclick={() => folds.reveal(group.key)}>Показать ещё {view.hidden} {fmt.plural(view.hidden, ['шаг', 'шага', 'шагов'])}</button>
          {/if}
          <div data-key={entry.key}>{@render row(entry)}</div>
        {/each}
      </div>
    </div>
  {/if}
</div>

<style>
  .steps { display: flex; flex-direction: column; min-width: 0; }
  .surface { border-radius: var(--radius-card); background: var(--raised); }
  .fold { display: grid; grid-template-rows: 1fr; animation: fold var(--duration-expand) var(--ease-expand); }
  .rows { display: flex; flex-direction: column; gap: var(--s2); min-height: 0; min-width: 0; overflow: hidden; padding-top: var(--s2); }
  .rows > div { min-width: 0; }
  .more { align-self: flex-start; min-height: var(--target); padding-inline: var(--s6); color: var(--accent-link); font-size: var(--fs-sm); }
  @keyframes fold { from { grid-template-rows: 0fr; } to { grid-template-rows: 1fr; } }
</style>
