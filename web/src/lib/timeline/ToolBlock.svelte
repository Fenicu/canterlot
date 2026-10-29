<script lang="ts">
  import type { BlockModel, BlockPage } from '$lib/stores/blocks';
  import { toolStatusText } from '$lib/timeline/present';
  import ToolOutput from '$lib/timeline/ToolOutput.svelte';

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
    <span class="name">{block.toolName || 'инструмент'}</span>
    <span class="summary">{block.summary || ''}</span>
    {#if isCliPolicy}
      <span class="policy-badge">Разрешено правилами CLI</span>
    {/if}
    <span class="pill" data-state={error ? 'error' : running ? 'running' : 'done'}>{status}</span>
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
    <div class="body">
      <ToolOutput {block} {load} />
    </div>
  {/if}
</div>

<style>
  .tool { display: flex; flex-direction: column; gap: var(--s2); }
  .head { display: flex; gap: var(--s2); align-items: baseline; text-align: left; transition-duration: var(--duration-expand); }
  .name, .summary { font-family: var(--font-mono); overflow-wrap: anywhere; }
  .policy-badge {
    font-size: var(--fs-xs);
    color: var(--secondary);
    background: var(--surface);
    border: var(--hairline) solid var(--control-line);
    border-radius: var(--radius-sm);
    padding: var(--s0) var(--s2);
    white-space: nowrap;
  }
  .pill[data-state='error'] { color: var(--danger); }
  .pill[data-state='running'] { color: var(--accent-link); }
  .body { transition-duration: var(--duration-expand); }
  .shots { display: flex; gap: var(--s2); overflow-x: auto; }
  .shot { margin: 0; overflow: hidden; }
  img { display: block; max-width: 100%; height: auto; }
</style>
