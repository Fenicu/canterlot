<script lang="ts">
  import { fmt } from '#lib/format/ru.js';
  import type { AttachmentRef } from '#lib/stores/inputs.js';
  import { attachmentKind, attachmentUrl, showsThumb } from '#lib/timeline/attachments.js';
  import Icon from '#lib/ui/Icon.svelte';
  import type { Component } from 'svelte';

  let { file, onopen }: { file: AttachmentRef; onopen: () => void } = $props();
  let broken = $state(false);
  // Просмотрщик документа грузится при первом открытии: в ленте он нужен редко.
  let DocViewer = $state<Component<{ file: AttachmentRef; onclose: () => void }> | null>(null);
  let reading = $state(false);

  const kind = $derived(attachmentKind(file));
  const name = $derived(file.name || 'Вложение');
  const url = $derived(attachmentUrl(file.attachment_id));
  const thumb = $derived(showsThumb(file) && !broken);
  const size = $derived(file.size > 0 ? fmt.bytes(file.size) : '');
  const extension = $derived.by(() => {
    const dot = name.lastIndexOf('.');
    return dot > 0 ? name.slice(dot + 1, dot + 5).toLocaleUpperCase('ru-RU') : 'ФАЙЛ';
  });

  async function openDocument(): Promise<void> {
    DocViewer ??= (await import('#lib/timeline/DocumentViewer.svelte')).default;
    reading = true;
  }
</script>

{#if thumb}
  <button type="button" class="thumb" aria-label={`Открыть картинку ${name}`} onclick={onopen}>
    <img src={url} alt="" loading="lazy" decoding="async" fetchpriority="low" onerror={() => { broken = true; }} />
  </button>
{:else if kind === 'image'}
  <button type="button" class="tile" aria-label={`Открыть картинку ${name}`} onclick={onopen}>
    <span class="glyph" aria-hidden="true">{extension}</span>
    <span class="info"><span class="name">{name}</span>{#if size}<span class="meta">{size}</span>{/if}</span>
  </button>
{:else if kind === 'markdown'}
  <span class="document-tile">
    <button type="button" class="tile" data-kind={kind} aria-label={`Открыть документ ${name}`} onclick={() => void openDocument()}>
      <span class="glyph" aria-hidden="true">{extension}</span>
      <span class="info"><span class="name">{name}</span>{#if size}<span class="meta">{size}</span>{/if}</span>
    </button>
    <!-- Адрес служебный, маршрутизатор здесь не участвует. -->
    <!-- eslint-disable-next-line svelte/no-navigation-without-resolve -->
    <a class="save" href={url} download={name} aria-label={`Скачать ${name}`}><Icon name="download" /></a>
  </span>
  {#if reading && DocViewer}
    <DocViewer {file} onclose={() => { reading = false; }} />
  {/if}
{:else}
  <!-- Адрес служебный, маршрутизатор здесь не участвует. -->
  <!-- eslint-disable-next-line svelte/no-navigation-without-resolve -->
  <a class="tile" data-kind={kind} href={url} download={name}>
    <span class="glyph" aria-hidden="true">{#if kind === 'video'}<Icon name="play" />{:else}{extension}{/if}</span>
    <span class="info"><span class="name">{name}</span>{#if size}<span class="meta">{size}</span>{/if}</span>
  </a>
{/if}

<style>
  .thumb { display: block; flex: none; width: calc(var(--s15) * 2.5); max-width: 100%; aspect-ratio: 1; padding: var(--s0); overflow: hidden; border-radius: var(--radius-sm); background: var(--chat-bg); }
  img { display: block; width: 100%; height: 100%; object-fit: cover; }
  .tile { display: flex; align-items: center; gap: var(--s4); min-width: 0; max-width: 100%; min-height: var(--target); padding: var(--s2) var(--s6) var(--s2) var(--s2); border-radius: var(--radius-sm); background: var(--raised); color: inherit; text-align: left; }
  .document-tile { display: flex; align-items: center; gap: var(--s2); min-width: 0; max-width: 100%; }
  .document-tile .tile { flex: 1 1 auto; }
  .save { display: inline-flex; align-items: center; justify-content: center; flex: none; width: var(--target); height: var(--target); border-radius: var(--radius-round); background: var(--chat-bg); color: var(--secondary); }
  .glyph { display: inline-flex; align-items: center; justify-content: center; flex: none; width: var(--s14); height: var(--s14); border-radius: var(--radius-sm); background: var(--code); color: var(--secondary); font-size: var(--fs-xs); font-weight: var(--weight-semibold); }
  .tile[data-kind='video'] .glyph { background: var(--chat-bg); color: var(--accent-link); }
  .glyph :global(svg) { width: var(--icon); height: var(--icon); fill: currentColor; }
  .info { display: flex; flex-direction: column; gap: var(--s1); min-width: 0; }
  .name { overflow: hidden; font-size: var(--fs-meta); text-overflow: ellipsis; white-space: nowrap; }
  .meta { color: var(--secondary); font-size: var(--fs-sm); }
</style>
