<script lang="ts">
  import type { Component } from 'svelte';
  import { fmt } from '#lib/format/ru.js';
  import AttachmentTile from '#lib/timeline/AttachmentTile.svelte';
  import { attachmentUrl } from '#lib/timeline/attachments.js';
  import CardLine from './CardLine.svelte';
  import CardShell from './CardShell.svelte';
  import type { ShareData } from './defs/share';
  import type { CardProps } from './types';

  let { call, data, line, link, word, env }: CardProps<ShareData> = $props();
  // Просмотрщик открывается так же, как у вложений сообщения и у обычного блока: грузится при первом касании.
  let Viewer = $state<Component<{ shots: { media_id: string; src: string; mime: string; caption: string }[]; start?: number; title: string; heading: string; onclose: () => void }> | null>(null);
  let viewing = $state<number | null>(null);
  const shots = $derived(
    data.files
      .filter((file) => file.mime.startsWith('image/'))
      .map((file) => ({
        media_id: file.attachment_id,
        src: attachmentUrl(file.attachment_id),
        mime: file.mime,
        caption: [file.title || file.name, file.size > 0 ? fmt.bytes(file.size) : ''].filter(Boolean).join(' · ')
      }))
  );

  async function view(id: string): Promise<void> {
    Viewer ??= (await import('#lib/timeline/MediaViewer.svelte')).default;
    viewing = Math.max(0, shots.findIndex((shot) => shot.media_id === id));
  }
</script>

<CardShell {call} {env} {line} status={word?.word ?? ''} tone={word?.tone ?? 'muted'}>
  {#snippet head()}<CardLine {line} {link} />{/snippet}
  {#if data.files.length > 0}
    <ul class="files">
      {#each data.files as file (file.attachment_id)}
        <li>
          <AttachmentTile file={{ attachment_id: file.attachment_id, name: file.name, mime: file.mime, size: file.size, sha256: '', image: file.mime.startsWith('image/') }} onopen={() => void view(file.attachment_id)} />
        </li>
      {/each}
    </ul>
  {:else if data.path && call.state === 'running'}
    <p class="meta">Передаётся {data.path}</p>
  {/if}
  {#if data.note}<p class="note" data-role="share-note">{data.note}</p>{/if}
  {#if data.error}<p class="danger">{data.error}</p>{/if}
</CardShell>
{#if viewing !== null && Viewer}
  <Viewer {shots} start={viewing} title={data.title || data.files[0]?.name || 'Файл'} heading="Файл" onclose={() => { viewing = null; }} />
{/if}

<style>
  p { margin: var(--s0); overflow-wrap: anywhere; }
  .files { display: flex; flex-wrap: wrap; align-items: flex-start; gap: var(--s2); margin: var(--s0); padding: var(--s0); list-style: none; }
  .files li { display: flex; min-width: 0; max-width: 100%; }
  .meta { color: var(--secondary); }
  .note { color: var(--secondary); white-space: pre-wrap; }
  .danger { color: var(--danger); }
</style>
