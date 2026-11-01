<script lang="ts">
  import { untrack } from 'svelte';
  import Icon from '#lib/ui/Icon.svelte';
  import IconButton from '#lib/ui/IconButton.svelte';
  import Sheet from '#lib/ui/Sheet.svelte';

  interface Shot {
    media_id: string;
    /** Адрес, если картинка не из блока ответа: например вложение сообщения. */
    src?: string;
    mime?: string;
    width?: number;
    height?: number;
    /** Подпись под картинкой вместо размеров. */
    caption?: string;
  }

  let { shots, start = 0, title, heading = 'Картинки ответа', onclose }: { shots: Shot[]; start?: number; title: string; heading?: string; onclose: () => void } = $props();
  let index = $state(untrack(() => start));
  const shot = $derived(shots[index] ?? shots[0]!);
  const src = $derived(shot.src ?? `/api/v1/media/${shot.media_id}`);
  const format = $derived((shot.mime ?? '').split('/')[1]?.toLocaleUpperCase('ru-RU') ?? '');
</script>

<Sheet open title={heading} subtitle={title} {onclose}>
  <div class="viewer">
    <img {src} alt="" width={shot.width} height={shot.height} />
    {#if shots.length > 1}
      <div class="nav">
        <IconButton filled icon="back" label="Предыдущая картинка" inert={index === 0} onclick={() => { index -= 1; }} />
        <span>{index + 1} из {shots.length}</span>
        <IconButton filled icon="chevron" label="Следующая картинка" inert={index === shots.length - 1} onclick={() => { index += 1; }} />
      </div>
    {/if}
    <div class="info">
      <span>{shot.caption ?? `${shot.width} × ${shot.height}${format ? ` · ${format}` : ''}`}</span>
      <!-- Адрес служебный, маршрутизатор здесь не участвует. -->
      <!-- eslint-disable-next-line svelte/no-navigation-without-resolve -->
      <a href={src} download><Icon name="download" />Сохранить</a>
    </div>
  </div>
</Sheet>

<style>
  .viewer { display: flex; flex-direction: column; align-items: center; gap: var(--s6); min-width: 0; }
  img { display: block; max-width: 100%; max-height: 60dvh; width: auto; height: auto; border-radius: var(--radius-sm); object-fit: contain; }
  .nav { display: flex; align-items: center; gap: var(--s6); font-size: var(--fs-meta); }
  .info { display: flex; flex-wrap: wrap; align-items: center; justify-content: space-between; gap: var(--s4); width: 100%; color: var(--secondary); font-size: var(--fs-sm); }
  a { display: inline-flex; align-items: center; gap: var(--s4); min-height: var(--target); padding-inline: var(--s8); border-radius: var(--radius-pill); background: var(--raised); color: var(--text); font-size: var(--fs-meta); }
</style>
