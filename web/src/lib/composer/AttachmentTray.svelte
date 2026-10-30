<script lang="ts">
  import type { Upload, UploadManager } from '#lib/uploads/manager.js';

  let { uploads, conversationId }: { uploads: UploadManager; conversationId: string } = $props();
  let items = $state<Upload[]>([]);

  $effect(() => {
    const id = conversationId;
    const pull = () => {
      items = uploads.list(id).filter((item) => item.phase !== 'cancelled');
    };
    pull();
    const timer = setInterval(pull, 250);
    return () => clearInterval(timer);
  });

  function sizeLabel(bytes: number): string {
    if (bytes >= 1_048_576) return `${Math.round(bytes / 1_048_576)} МиБ`;
    if (bytes >= 1024) return `${Math.round(bytes / 1024)} КиБ`;
    return `${bytes} Б`;
  }
</script>

{#if items.length > 0}
  <ul class="tray">
    {#each items as item (item.id)}
      <li>
        <span class="name">{item.name}</span>
        {#if item.size > 0}<span class="size">{sizeLabel(item.size)}</span>{/if}
        <span class="bar" role="progressbar" aria-valuemin={0} aria-valuenow={item.phase === 'ready' ? item.size : item.sent} aria-valuemax={Math.max(item.size, 1)} aria-label={item.name}></span>
        {#if item.error}<p class="error">{item.error.message}</p>{/if}
        {#if item.phase === 'failed'}
          <button type="button" onclick={() => uploads.retry(item.id)}>Повторить</button>
        {/if}
        <button type="button" onclick={() => uploads.cancel(item.id)}>Убрать</button>
      </li>
    {/each}
  </ul>
{/if}

<style>
  .tray { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: var(--s2); }
  li { display: flex; flex-wrap: wrap; align-items: center; gap: var(--s2); min-width: 0; }
  .name { overflow-wrap: anywhere; }
  .size { color: var(--muted); font-size: var(--fs-sm); }
  .bar { flex: 1 1 100%; height: var(--s2); border-radius: var(--radius-pill); background: var(--accent-soft); }
  .error { margin: 0; color: var(--danger); font-size: var(--fs-sm); }
  button { min-height: var(--target); border: var(--s0); background: var(--transparent); color: var(--secondary); font-size: var(--fs-meta); }
</style>
