<script lang="ts">
  import type { BlockModel, BlockPage } from '#lib/stores/blocks.js';
  import { toolStatusText } from '#lib/timeline/present.js';
  import ToolOutput from '#lib/timeline/ToolOutput.svelte';
  import Icon from '#lib/ui/Icon.svelte';

  let {
    block,
    now,
    load
  }: {
    block: BlockModel;
    now: number;
    load: (blockId: string, offset: number) => Promise<BlockPage>;
  } = $props();
  let open = $state(false);
  const running = $derived(block.state === 'streaming' || block.state === 'stale');
  const error = $derived(block.state === 'incomplete' || block.kind === 'error');
  const status = $derived(toolStatusText({
    running: block.state === 'streaming',
    error,
    startedAt: block.startedAt ?? block.createdAt,
    finishedAt: block.finishedAt,
    now
  }));
  const shots = $derived(block.media);
  const isCliPolicy = $derived(
    Boolean(
      (block as unknown as { approved_by?: string; approvedBy?: string }).approved_by === 'cli_policy' ||
        (block as unknown as { approved_by?: string; approvedBy?: string }).approvedBy === 'cli_policy'
    )
  );
</script>

<div class="tool" data-open={open ? '' : undefined}>
  <button type="button" class="head" aria-expanded={open} onclick={() => { open = !open; }}>
    <span class="chevron"><Icon name="chevron" /></span>
    <span class="name">{block.toolName || 'инструмент'}</span>
    <span class="summary">{block.summary || ''}</span>
    {#if isCliPolicy}
      <span class="policy-badge">Разрешено правилами CLI</span>
    {/if}
    {#if status}<span class="pill" data-state={error ? 'error' : running ? 'running' : 'done'}>{status}</span>{/if}
  </button>
  {#if shots.length > 0}
    <div class="shots">
      {#each shots as shot (shot.media_id)}
        <figure class="shot" style:width={`${shot.width}px`} style:height={`${shot.height}px`}>
          {#if shot.ready}
            <img src={`/api/v1/media/${shot.media_id}`} alt="" width={shot.width} height={shot.height} />
          {:else if shot.failed}
            <span>картинка недоступна</span>
          {:else}
            <span>картинка загружается</span>
          {/if}
        </figure>
      {/each}
    </div>
  {/if}
  {#if open}
    <div class="fold">
      <div class="body">
        <ToolOutput {block} {load} />
      </div>
    </div>
  {/if}
</div>

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
  .name { flex: none; font-family: var(--font-mono); font-weight: var(--weight-semibold); }
  .summary { flex: 1; min-width: 0; overflow: hidden; color: var(--secondary); font-family: var(--font-mono); text-overflow: ellipsis; white-space: nowrap; }
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
  .shots { display: flex; gap: var(--s2); overflow-x: auto; }
  .shot { margin: 0; overflow: hidden; }
  img { display: block; max-width: 100%; height: auto; }
</style>
