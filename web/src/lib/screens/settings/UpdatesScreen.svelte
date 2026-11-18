<script lang="ts">
  import { onDestroy, onMount } from 'svelte';
  import { browserRuntime } from '#lib/app/browser.js';
  import type { Runtime } from '#lib/app/runtime.js';
  import { fmt } from '#lib/format/ru.js';
  import { COPY_DONE, COPY_FAILED } from '#lib/render/copy.js';
  import {
    isVersionKind, updateCommand, updateHarness, updateText, updateTone,
    type UpdateItem
  } from '#lib/updates/text.js';
  import Button from '#lib/ui/Button.svelte';
  import Select from '#lib/ui/Select.svelte';

  let runtime = $state<Runtime | null>(null);
  let machine = $state('');
  let kind = $state('');
  let sentinel = $state<HTMLElement | null>(null);
  let copyStatus = $state('');
  let copyFor = $state('');
  let copyTimer: ReturnType<typeof setTimeout> | null = null;
  let disposed = false;

  const updates = $derived(runtime?.updates ?? null);
  const machineOptions = $derived([
    { value: '', label: 'Все машины' },
    ...[...new Set([
      ...(runtime ? [...runtime.machines.items.values()].map((entry) => entry.name) : []),
      ...(updates?.machines ?? [])
    ])].map((name) => ({ value: name, label: name }))
  ]);
  const kindOptions = [
    { value: '', label: 'Все события' },
    { value: 'version_available', label: 'Доступна версия' },
    { value: 'below_minimum', label: 'Ниже минимума' },
    { value: 'cli_updated', label: 'CLI обновился' },
    { value: 'models_added', label: 'Модели добавлены' },
    { value: 'models_removed', label: 'Модели убраны' }
  ];

  function applyFilters() {
    void updates?.load({ machine: machine || undefined, kind: kind || undefined });
  }

  async function copy(item: UpdateItem) {
    const command = updateCommand(item);
    if (!command) return;
    try {
      if (typeof navigator === 'undefined' || !navigator.clipboard?.writeText) throw new Error('clipboard_unavailable');
      await navigator.clipboard.writeText(command);
      copyStatus = COPY_DONE;
      copyFor = item.id;
    } catch {
      copyStatus = COPY_FAILED;
      copyFor = item.id;
    }
    if (copyTimer) clearTimeout(copyTimer);
    copyTimer = setTimeout(() => { copyStatus = ''; copyFor = ''; copyTimer = null; }, 2000);
  }

  onMount(() => {
    void browserRuntime().then((rt) => {
      if (disposed) return;
      runtime = rt;
      void rt.updates.load();
    });
  });
  onDestroy(() => {
    disposed = true;
    if (copyTimer) clearTimeout(copyTimer);
  });

  $effect(() => {
    const node = sentinel;
    if (!node || typeof IntersectionObserver === 'undefined') return;
    const observer = new IntersectionObserver((entries) => {
      if (entries.some((entry) => entry.isIntersecting)) void updates?.loadMore();
    }, { rootMargin: '240px' });
    observer.observe(node);
    return () => observer.disconnect();
  });

  function stamp(createdAt: string): string {
    const at = Date.parse(createdAt);
    return Number.isFinite(at) ? fmt.stamp(at, Date.now()) : createdAt;
  }
</script>

<div class="feed" data-update-feed>
  <div class="filters">
    <Select label="Машина" options={machineOptions} bind:value={machine} onchange={applyFilters} />
    <Select label="Событие" options={kindOptions} bind:value={kind} onchange={applyFilters} />
  </div>
  {#if updates?.error}<p class="error" role="alert">{updates.error}<button type="button" onclick={applyFilters}>Повторить</button></p>{/if}
  {#if !updates || (updates.loading && updates.items.length === 0)}
    <p class="empty" role="status">Загружаю ленту обновлений…</p>
  {:else if updates.items.length === 0}
    <p class="empty" data-update-empty>Пока нет обновлений и изменений моделей</p>
  {:else}
    <ol class="rows">
      {#each updates.items as item (item.id)}
        <li class="row" data-update-item data-update-kind={item.kind} data-tone={updateTone(item.kind)}>
          <p class="meta"><span>{stamp(item.created_at)}</span><span class="dot">·</span><span>{updateHarness(item.harness)}</span></p>
          <p class="line">{updateText(item)}</p>
          <p class="where">{item.machine}</p>
          {#if isVersionKind(item.kind) && updateCommand(item)}
            <div class="command">
              <code data-update-command>{updateCommand(item)}</code>
              <div class="copy-line">
                <Button onclick={() => void copy(item)}>Копировать</Button>
                {#if copyFor === item.id && copyStatus}<span class="status" role="status">{copyStatus}</span>{/if}
              </div>
            </div>
          {/if}
          {#if (item.payload.models?.length ?? 0) > 5}
            <details class="models">
              <summary>Все модели ({item.payload.models!.length})</summary>
              <p>{item.payload.models!.join(', ')}</p>
            </details>
          {/if}
        </li>
      {/each}
    </ol>
    {#if updates.nextCursor}
      <div bind:this={sentinel} class="more">
        <Button onclick={() => void updates!.loadMore()}>Загрузить ещё</Button>
      </div>
    {/if}
    {#if updates.loading}<p class="more-note" role="status">Загружаю ещё…</p>{/if}
  {/if}
</div>

<style>
  .feed { display: flex; flex-direction: column; gap: var(--s4); }
  .filters { display: grid; grid-template-columns: repeat(auto-fit, minmax(min(100%, var(--settings-button)), 1fr)); gap: var(--s4); padding: var(--s2) var(--s4); }
  .rows { display: flex; flex-direction: column; gap: var(--s3); list-style: none; margin: var(--s0); padding: var(--s0); }
  .row { padding: var(--s5) var(--s6); border-radius: var(--radius-card); background: var(--surface); }
  .row[data-tone='warning'] { background: var(--warning-soft); }
  .meta { display: flex; flex-wrap: wrap; gap: var(--s2); margin: var(--s0) var(--s0) var(--s2); color: var(--muted); font-size: var(--fs-sm); }
  .dot { color: var(--secondary); }
  .line { margin: var(--s0); color: var(--text); overflow-wrap: anywhere; }
  .row[data-tone='warning'] .line { color: var(--warning); }
  .where { margin: var(--s2) var(--s0) var(--s0); color: var(--secondary); font-size: var(--fs-sm); }
  .command { display: flex; flex-direction: column; gap: var(--s3); margin-top: var(--s4); }
  code { display: block; overflow-x: auto; padding: var(--s4); border-radius: var(--radius-sm); background: var(--raised); color: var(--text); font-family: var(--font-mono); font-size: var(--fs-sm); white-space: pre; }
  .copy-line { display: flex; align-items: center; gap: var(--s4); }
  .status { color: var(--secondary); font-size: var(--fs-sm); }
  .models { margin-top: var(--s3); color: var(--secondary); font-size: var(--fs-sm); }
  .models summary { cursor: pointer; min-height: var(--target); }
  .models p { margin: var(--s2) var(--s0) var(--s0); overflow-wrap: anywhere; }
  .empty { margin: var(--s0); padding: var(--s8) var(--s6); border-radius: var(--radius-card); background: var(--surface); color: var(--secondary); }
  .error { display: flex; flex-wrap: wrap; align-items: center; gap: var(--s4); margin: var(--s0); color: var(--danger); }
  .more { display: flex; justify-content: center; padding-block: var(--s4); }
  .more-note { margin: var(--s0); color: var(--muted); font-size: var(--fs-sm); text-align: center; }
</style>
