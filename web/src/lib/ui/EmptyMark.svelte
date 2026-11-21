<script lang="ts">
  import type { IconName } from './icons';
  import InkIcon from './InkIcon.svelte';

  /**
   * Значок пустого экрана: тот же, что у семейства вещей в ленте и в списках. Прописывается
   * чернилами один раз, когда экран показан, и дальше стоит: ничего не крутится на пустом месте.
   * Без движения (системного или масштабом 0) стоит написанным сразу. `inline` ставит значок в строку
   * с текстом, чтобы место под ним не росло; без него значок стоит над фразой.
   */
  let { name, inline = false }: { name: IconName; inline?: boolean } = $props();
</script>

<span class="empty-mark" class:inline data-empty-mark={name}><InkIcon {name} state="ink" size={inline ? 'small' : 'normal'} /></span>

<style>
  .empty-mark { display: flex; margin-block-end: var(--s4); --ink-rest: var(--muted); }
  .empty-mark.inline { display: inline-flex; flex: none; margin: var(--s0); }
</style>
