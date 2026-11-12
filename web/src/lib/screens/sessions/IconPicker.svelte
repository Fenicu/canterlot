<script lang="ts">
  import Icon from '#lib/ui/Icon.svelte';
  import { GROUP_ICONS, groupIcon, type GroupIcon } from './groupIcons';

  let { value, label = 'Значок группы', onpick }: { value: string | null | undefined; label?: string; onpick: (icon: GroupIcon) => void } = $props();
  const chosen = $derived(groupIcon(value));
  let grid = $state<HTMLElement | null>(null);

  // Стрелки двигают выбор по сетке, как в группе переключателей.
  function move(event: KeyboardEvent) {
    const step = event.key === 'ArrowRight' || event.key === 'ArrowDown' ? 1 : event.key === 'ArrowLeft' || event.key === 'ArrowUp' ? -1 : 0;
    if (!step) return;
    event.preventDefault();
    const index = GROUP_ICONS.findIndex((icon) => icon.id === chosen);
    const next = GROUP_ICONS[(index + step + GROUP_ICONS.length) % GROUP_ICONS.length];
    if (!next) return;
    onpick(next.id);
    queueMicrotask(() => grid?.querySelector<HTMLElement>('[aria-checked="true"]')?.focus());
  }
</script>

<div bind:this={grid} class="picker" role="radiogroup" aria-label={label} data-icon-picker tabindex="-1" onkeydown={move}>
  {#each GROUP_ICONS as icon (icon.id)}
    <button
      type="button"
      role="radio"
      aria-checked={icon.id === chosen}
      aria-label={icon.label}
      title={icon.label}
      tabindex={icon.id === chosen ? 0 : -1}
      data-icon={icon.id}
      onclick={() => onpick(icon.id)}
    ><Icon name={icon.id} /></button>
  {/each}
</div>

<style>
  .picker { display: grid; grid-template-columns: repeat(8, minmax(0, 1fr)); gap: var(--s2); flex: 1 1 100%; }
  button { display: inline-flex; align-items: center; justify-content: center; min-height: var(--target); border: var(--s0); border-radius: var(--radius-sm); background: var(--transparent); color: var(--secondary); }
  @media (hover: hover) and (pointer: fine) { button:hover { background: var(--hover); } }
  button[aria-checked='true'] { background: var(--accent); color: var(--accent-ink); }
  button:focus-visible { outline: var(--focus-width) solid var(--accent); outline-offset: var(--s1); }
</style>
