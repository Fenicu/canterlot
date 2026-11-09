<script lang="ts">
  import { fmt } from '#lib/format/ru.js';
  import { ticker } from '#lib/ticker.svelte.js';
  import type { Upload, UploadManager } from '#lib/uploads/manager.js';
  import Button from '#lib/ui/Button.svelte';
  import IconButton from '#lib/ui/IconButton.svelte';

  let { uploads, conversationId }: { uploads: UploadManager; conversationId: string } = $props();
  let items = $state<Upload[]>([]);

  $effect(() => ticker.retain());

  $effect(() => {
    void ticker.now;
    items = uploads.list(conversationId).filter((item) => item.phase !== 'cancelled');
  });

  const ready = $derived(items.filter((item) => item.phase === 'ready').length);
  const failed = $derived(items.filter((item) => item.phase === 'failed').length);
  const tally = $derived([ready ? `${ready} готово` : '', failed ? `${failed} ${fmt.plural(failed, ['ошибка', 'ошибки', 'ошибок'])}` : ''].filter(Boolean).join(' · '));

  function sizeLabel(bytes: number): string {
    if (bytes >= 1_048_576) return `${Math.round(bytes / 1_048_576)} МиБ`;
    if (bytes >= 1024) return `${Math.round(bytes / 1024)} КиБ`;
    return `${bytes} Б`;
  }

  function extension(name: string): string {
    const dot = name.lastIndexOf('.');
    return dot > 0 ? name.slice(dot + 1, dot + 5).toLocaleUpperCase('ru-RU') : 'ФАЙЛ';
  }

  function percent(item: Upload): number {
    // Пустой файл передан целиком, едва начавшись: тел нет, остаётся только завершение.
    if (item.phase === 'ready' || item.size === 0) return 100;
    return Math.round((item.sent / Math.max(item.size, 1)) * 100);
  }

  function statusOf(item: Upload): string {
    if (item.phase === 'ready') return 'Загружено';
    if (item.phase === 'failed') return 'сбой загрузки';
    return `Загрузка · ${percent(item)}%`;
  }
</script>

{#if items.length > 0}
  <section class="tray" aria-label="Вложения">
    <header><strong>Вложения · {items.length}</strong>{#if tally}<span>{tally}</span>{/if}</header>
    <ul>
      {#each items as item (item.id)}
        <li data-phase={item.phase}>
          <span class="tile" aria-hidden="true">{extension(item.name)}</span>
          <div class="info">
            <span class="name">{item.name}</span>
            <span class="meta">{`${sizeLabel(item.size)} · `}<span class="status">{statusOf(item)}</span></span>
            <span class="bar" role="progressbar" aria-valuemin={0} aria-valuenow={item.phase === 'ready' || item.size === 0 ? Math.max(item.size, 1) : item.sent} aria-valuemax={Math.max(item.size, 1)} aria-label={item.name}><span class="fill" style:width={`${percent(item)}%`}></span></span>
            {#if item.error}<p class="error">{item.error.message}</p>{:else if item.note && item.phase !== 'ready'}<p class="note">{item.note}</p>{/if}
            {#if item.phase === 'failed'}
              <span class="retry"><Button onclick={() => uploads.retry(item.id)}>Повторить</Button></span>
            {/if}
          </div>
          <IconButton icon="close" label="Убрать" onclick={() => uploads.cancel(item.id)} />
        </li>
      {/each}
    </ul>
  </section>
{/if}

<style>
  .tray { grid-area: tray; display: flex; flex-direction: column; gap: var(--s3); min-width: 0; max-height: calc(var(--composer-limit) * .36); overflow-y: auto; padding: var(--s1) var(--s3) var(--s3); }
  header { display: flex; align-items: baseline; justify-content: space-between; gap: var(--s4); padding-inline: var(--s3); font-size: var(--fs-meta); }
  strong { font-weight: var(--weight-semibold); }
  header span { color: var(--secondary); font-size: var(--fs-sm); }
  ul { display: flex; flex-direction: column; gap: var(--s2); margin: 0; padding: 0; list-style: none; }
  li { display: flex; align-items: center; gap: var(--s4); min-width: 0; padding: var(--s3); border-radius: var(--radius-card); background: var(--chat-bg); }
  li[data-phase='failed'] { background: var(--danger-soft); }
  .tile { display: inline-flex; align-items: center; justify-content: center; flex: none; width: var(--s14); height: var(--s14); border-radius: var(--radius-sm); background: var(--code); color: var(--secondary); font-size: var(--fs-xs); font-weight: var(--weight-semibold); }
  .info { display: flex; flex: 1; flex-direction: column; gap: var(--s1); min-width: 0; }
  .name { overflow: hidden; font-size: var(--fs-meta); text-overflow: ellipsis; white-space: nowrap; }
  .meta { color: var(--secondary); font-size: var(--fs-sm); }
  .note { margin: 0; color: var(--secondary); font-size: var(--fs-sm); }
  [data-phase='ready'] .status { color: var(--success); }
  [data-phase='failed'] .status, .error { color: var(--danger); }
  .bar { display: block; height: var(--s1); overflow: hidden; border-radius: var(--radius-pill); background: var(--control-line); }
  .fill { display: block; height: 100%; border-radius: var(--radius-pill); background: var(--accent); }
  [data-phase='ready'] .bar { height: var(--hairline); background: var(--transparent); }
  [data-phase='ready'] .fill { opacity: 0; }
  [data-phase='failed'] .fill { background: var(--danger); }
  .error { margin: 0; font-size: var(--fs-sm); }
  .retry { display: flex; }
  /* Клавиатура открыта: вложения едут одной строкой вбок, чтобы поле ввода осталось видно. */
  :global(html[data-compact]) .tray { max-height: none; gap: var(--s2); padding: var(--s0) var(--s3) var(--s2); overflow: visible; }
  :global(html[data-compact]) header { display: none; }
  :global(html[data-compact]) ul { flex-direction: row; overflow-x: auto; padding-bottom: var(--s1); }
  :global(html[data-compact]) li { flex: 0 0 auto; max-width: calc(var(--target) * 5); min-height: var(--target); padding: var(--s2) var(--s3); }
  :global(html[data-compact]) .tile { width: var(--s10); height: var(--s10); }
  :global(html[data-compact]) .meta,
  :global(html[data-compact]) .bar { display: none; }
</style>
