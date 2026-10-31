<script lang="ts">
  import type { Component } from 'svelte';
  import type { BlockMedia, BlockModel, BlockPage } from '#lib/stores/blocks.js';
  import { toolFailed, toolStatusText, toolTiming, toolTitle } from '#lib/timeline/present.js';
  import ToolOutput from '#lib/timeline/ToolOutput.svelte';
  import Icon from '#lib/ui/Icon.svelte';

  let {
    block,
    result = null,
    working = false,
    now,
    load
  }: {
    block: BlockModel;
    result?: BlockModel | null;
    working?: boolean;
    now: number;
    load: (blockId: string, offset: number) => Promise<BlockPage>;
  } = $props();
  let open = $state(false);
  let viewing = $state<number | null>(null);
  let Viewer = $state<Component<{ shots: BlockMedia[]; start?: number; title: string; onclose: () => void }> | null>(null);

  async function view(index: number): Promise<void> {
    Viewer ??= (await import('#lib/timeline/MediaViewer.svelte')).default;
    viewing = index;
  }
  const title = $derived(toolTitle(block));
  const detail = $derived([title.server, title.argument].filter(Boolean).join(' · '));
  const timing = $derived(toolTiming(block, result, working));
  const running = $derived(timing.running || block.state === 'stale');
  const error = $derived(block.state === 'incomplete' || block.kind === 'error' || toolFailed(result ?? (block.kind === 'tool_result' ? block : null)));
  const status = $derived(toolStatusText({ ...timing, error, now }));
  const shots = $derived(block.media);
  const readyShots = $derived(shots.filter((shot) => shot.ready));
  const isCliPolicy = $derived(block.approvedBy === 'cli_policy');
</script>

<div class="tool" data-open={open ? '' : undefined}>
  <button type="button" class="head" aria-expanded={open} onclick={() => { open = !open; }}>
    <span class="chevron"><Icon name="chevron" /></span>
    <span class="name">{title.name || 'Результат инструмента'}</span>
    {#if detail}<span class="summary">{detail}</span>{/if}
    {#if isCliPolicy}
      <span class="policy-badge">Разрешено правилами CLI</span>
    {/if}
    {#if status}<span class="pill" data-state={error ? 'error' : running ? 'running' : 'done'}>{status}</span>{/if}
  </button>
  {#if shots.length > 0}
    <div class="shots">
      {#each shots as shot (shot.media_id)}
        <figure class="shot" data-state={shot.ready ? 'ready' : shot.failed ? 'failed' : 'loading'} style:max-width={`${shot.width}px`} style:aspect-ratio={`${shot.width} / ${shot.height}`}>
          {#if shot.ready}
            <button type="button" aria-label="Открыть картинку" onclick={() => void view(readyShots.indexOf(shot))}>
              <img src={`/api/v1/media/${shot.media_id}`} alt="" width={shot.width} height={shot.height} loading="lazy" />
            </button>
          {:else if shot.failed}
            <span>Картинка недоступна</span>
          {:else}
            <span>Картинка загружается</span>
          {/if}
        </figure>
      {/each}
    </div>
  {/if}
  {#if open}
    <div class="fold">
      <div class="body">
        <ToolOutput block={result ?? block} running={timing.running} {load} />
      </div>
    </div>
  {/if}
</div>
{#if viewing !== null && Viewer}
  <Viewer shots={readyShots} start={viewing} title={title.argument || title.name || 'инструмент'} onclose={() => { viewing = null; }} />
{/if}

<style>
  .tool { display: flex; flex-direction: column; min-width: 0; border-radius: var(--radius-card); background: var(--raised); }
  .head {
    display: flex;
    gap: var(--s3);
    align-items: center;
    width: 100%;
    min-height: var(--target);
    padding: var(--s2) var(--s5) var(--s2) var(--s4);
    border-radius: var(--radius-card);
    background: var(--transparent);
    font-size: var(--fs-sm);
    text-align: left;
  }
  .chevron { flex: none; color: var(--muted); transition: transform var(--duration-expand) var(--ease-expand); }
  .chevron :global(svg) { width: var(--icon-small); height: var(--icon-small); }
  [data-open] .chevron { transform: rotate(90deg); }
  .name { flex: 0 1 auto; min-width: 0; max-width: 70%; overflow: hidden; font-family: var(--font-mono); font-weight: var(--weight-semibold); text-overflow: ellipsis; white-space: nowrap; }
  .summary { flex: 1 1 0; min-width: 0; overflow: hidden; color: var(--secondary); font-family: var(--font-mono); text-overflow: ellipsis; white-space: nowrap; }
  .policy-badge {
    flex: none;
    padding: var(--s1) var(--s3);
    border-radius: var(--radius-pill);
    background: var(--surface);
    color: var(--secondary);
    font-size: var(--fs-xs);
    white-space: nowrap;
  }
  .pill {
    flex: none;
    margin-left: auto;
    padding: var(--s1) var(--s4);
    border-radius: var(--radius-pill);
    background: var(--chip);
    color: var(--chip-text);
    font-size: var(--fs-sm);
    white-space: nowrap;
  }
  .pill[data-state='error'] { background: var(--danger-soft); color: var(--danger); }
  .pill[data-state='running'] { background: var(--accent-soft); color: var(--accent-link); }
  .fold { display: grid; grid-template-rows: 1fr; animation: fold var(--duration-expand) var(--ease-expand); }
  .body { min-height: 0; overflow: hidden; padding-inline: var(--s2); }
  .body > :global(:last-child) { margin-bottom: var(--s2); }
  @keyframes fold { from { grid-template-rows: 0fr; } to { grid-template-rows: 1fr; } }
  .shots { display: flex; flex-wrap: wrap; gap: var(--s2); padding: var(--s0) var(--s2) var(--s2); }
  .shot { flex: 1 1 calc(50% - var(--s2)); min-width: 0; max-height: var(--preview-height); margin: 0; overflow: hidden; border-radius: var(--radius-sm); background: var(--chat-bg); }
  .shot[data-state='failed'] { background: var(--danger-soft); }
  .shot:not([data-state='ready']) { display: flex; align-items: center; justify-content: center; min-height: var(--target); padding: var(--s4); color: var(--secondary); font-size: var(--fs-sm); text-align: center; }
  .shot[data-state='failed'] { color: var(--danger); }
  .shot button { display: block; width: 100%; height: 100%; padding: var(--s0); border-radius: var(--radius-sm); background: var(--transparent); }
  img { display: block; width: 100%; height: 100%; object-fit: cover; }
</style>
