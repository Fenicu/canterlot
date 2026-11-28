<script lang="ts">
  import { fmt } from '#lib/format/ru.js';
  import { ticker } from '#lib/ticker.svelte.js';
  import type { Upload, UploadManager } from '#lib/uploads/manager.js';
  import Button from '#lib/ui/Button.svelte';
  import Icon from '#lib/ui/Icon.svelte';
  import IconButton from '#lib/ui/IconButton.svelte';

  // canEdit и onedit приходят от композера: карандаш есть только у картинки, которую откроет редактор.
  let {
    uploads,
    conversationId,
    canEdit = () => false,
    onedit
  }: {
    uploads: UploadManager;
    conversationId: string;
    canEdit?: (id: string) => boolean;
    onedit?: (id: string) => void;
  } = $props();
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

  let strip = $state<HTMLUListElement | null>(null);
  let fadeStart = $state(false);
  let fadeEnd = $state(false);

  // В строке клавиатуры край гаснет только там, где за ним правда остались чипы.
  function measure() {
    if (!strip) return;
    fadeStart = strip.scrollLeft > 1;
    fadeEnd = strip.scrollLeft + strip.clientWidth < strip.scrollWidth - 1;
  }

  $effect(() => {
    // Список вырос или сменил направление — пересчитать края и подписаться на новые чипы.
    void items.length;
    const node = strip;
    if (!node) return;
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(node);
    for (const child of node.children) observer.observe(child);
    return () => observer.disconnect();
  });
</script>

{#if items.length > 0}
  <section class="tray" aria-label="Вложения">
    <header><strong>Вложения · {items.length}</strong>{#if tally}<span>{tally}</span>{/if}</header>
    <ul
      bind:this={strip}
      data-fade-start={fadeStart ? '' : undefined}
      data-fade-end={fadeEnd ? '' : undefined}
      onscroll={measure}
    >
      {#each items as item (item.id)}
        <li data-phase={item.phase}>
          <span class="thumb">
            <span class="tile" aria-hidden="true">{extension(item.name)}</span>
            {#if onedit && canEdit(item.id)}
              <button type="button" class="pencil" aria-label="Редактировать изображение" title="Редактировать изображение" onclick={() => onedit(item.id)}><span class="badge"><Icon name="edit" /></span></button>
            {/if}
          </span>
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
  .tray { grid-area: tray; display: flex; flex-direction: column; gap: var(--s3); min-width: 0; max-height: calc(var(--composer-limit) * .36); overflow-x: hidden; overflow-y: auto; scrollbar-width: none; padding: var(--s1) var(--s3) var(--s3); }
  .tray::-webkit-scrollbar { display: none; }
  header { display: flex; align-items: baseline; justify-content: space-between; gap: var(--s4); padding-inline: var(--s3); font-size: var(--fs-meta); }
  strong { font-weight: var(--weight-semibold); }
  header span { color: var(--secondary); font-size: var(--fs-sm); }
  ul { display: flex; flex-direction: column; gap: var(--s2); margin: 0; padding: 0; list-style: none; }
  li { display: flex; align-items: center; gap: var(--s4); min-width: 0; padding: var(--s3); border-radius: var(--radius-card); background: var(--chat-bg); }
  li[data-phase='failed'] { background: var(--danger-soft); }
  .tile { display: inline-flex; align-items: center; justify-content: center; flex: none; width: var(--s14); height: var(--s14); border-radius: var(--radius-sm); background: var(--code); color: var(--secondary); font-size: var(--fs-xs); font-weight: var(--weight-semibold); }
  .thumb { position: relative; display: inline-flex; flex: none; }
  /* Видимый значок меньше миниатюры, а нажимается вся область --target вокруг него: подсказки наведения на телефоне нет. */
  .pencil { position: absolute; right: calc(var(--s5) * -1); bottom: calc(var(--s5) * -1); display: inline-flex; align-items: center; justify-content: center; width: var(--target); height: var(--target); padding: var(--s0); border: var(--s0); background: var(--transparent); color: var(--text); border-radius: var(--radius-round); }
  /* Подпись типа уходит вверх, чтобы карандаш в углу её не закрывал. */
  .thumb:has(.pencil) .tile { align-items: flex-start; padding-top: var(--s3); }
  .pencil:active .badge { transform: scale(var(--scale-press)); }
  .badge { display: inline-flex; align-items: center; justify-content: center; width: var(--s10); height: var(--s10); border: var(--hairline) solid var(--control-line); border-radius: var(--radius-round); background: var(--surface); --icon: var(--s8); }
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
  :global(html[data-compact]) .tray { max-height: none; gap: var(--s2); padding: var(--s5) var(--s3) var(--s2); overflow: visible; }
  :global(html[data-compact]) header { display: none; }
  :global(html[data-compact]) ul { flex-direction: row; overflow-x: auto; scrollbar-width: none; scroll-padding-inline: var(--s6); padding-bottom: var(--s0); }
  :global(html[data-compact]) ul::-webkit-scrollbar { display: none; }
  :global(html[data-compact]) ul[data-fade-end] { mask-image: linear-gradient(to left, var(--transparent), var(--text) var(--s6)); }
  :global(html[data-compact]) ul[data-fade-start] { mask-image: linear-gradient(to right, var(--transparent), var(--text) var(--s6)); }
  :global(html[data-compact]) ul[data-fade-start][data-fade-end] { mask-image: linear-gradient(to right, var(--transparent), var(--text) var(--s6), var(--text) calc(100% - var(--s6)), var(--transparent)); }
  :global(html[data-compact]) li { flex: 0 0 auto; max-width: calc(var(--target) * 5); min-height: var(--target); padding: var(--s2) var(--s3); }
  :global(html[data-compact]) .tile { width: var(--s10); height: var(--s10); }
  :global(html[data-compact]) .meta,
  :global(html[data-compact]) .bar { display: none; }
</style>
