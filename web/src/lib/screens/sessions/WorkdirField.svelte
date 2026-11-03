<script lang="ts">
  import { suggestWorkdirs } from '#lib/api/client.js';
  import Field from '#lib/ui/Field.svelte';
  import { WorkdirSuggester } from './workdirSuggest';

  const ANSWER_TIMEOUT_MS = 4000;

  let { machineId, value = $bindable('') }: { machineId: string; value?: string } = $props();

  const listId = $props.id();
  let items = $state<string[]>([]);
  let active = $state(-1);
  const optionId = (index: number) => `${listId}-${index}`;
  const nameOf = (item: string) => item.slice(item.lastIndexOf('/') + 1);

  const suggester = new WorkdirSuggester(
    (machine, path, signal) => suggestWorkdirs(machine, path, { signal, timeoutMs: ANSWER_TIMEOUT_MS }).then((response) => response.data.workdirs),
    (next) => {
      items = next;
      active = -1;
    }
  );

  $effect(() => {
    void machineId;
    return () => suggester.clear();
  });

  // Список живёт в оверлее и высоту формы не меняет: сдвиг между нажатием и отпусканием исключён.
  let field: HTMLInputElement | null = $state(null);
  let anchor = $state({ top: 0, left: 0, width: 0, room: 0 });

  function place() {
    const box = field?.getBoundingClientRect();
    if (!box || typeof window === 'undefined') return;
    const edge = 8;
    const gap = 4;
    const width = Math.min(box.width, window.innerWidth - edge * 2);
    const left = Math.min(Math.max(box.left, edge), Math.max(edge, window.innerWidth - width - edge));
    anchor = { top: box.bottom + gap, left, width, room: Math.max(0, window.innerHeight - box.bottom - gap - edge) };
  }

  $effect(() => {
    if (items.length === 0) return;
    place();
    const update = () => place();
    window.addEventListener('resize', update);
    window.addEventListener('scroll', update, true);
    const outside = (event: PointerEvent) => {
      const target = event.target as Node | null;
      if (!target) return;
      if (field?.contains(target)) return;
      const listEl = document.getElementById(listId);
      if (listEl?.contains(target)) return;
      suggester.clear();
    };
    document.addEventListener('pointerdown', outside, true);
    return () => {
      window.removeEventListener('resize', update);
      window.removeEventListener('scroll', update, true);
      document.removeEventListener('pointerdown', outside, true);
    };
  });

  function pick(item: string) {
    value = `${item}/`;
    suggester.input(machineId, value, 0);
  }

  function keydown(event: KeyboardEvent) {
    if (items.length === 0 || event.isComposing) return;
    const chosen = items[active];
    if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
      event.preventDefault();
      const step = event.key === 'ArrowDown' ? 1 : -1;
      active = active < 0 && step < 0 ? items.length - 1 : (active + step + items.length) % items.length;
      document.getElementById(optionId(active))?.scrollIntoView({ block: 'nearest' });
    } else if (event.key === 'Enter' && chosen) {
      event.preventDefault();
      pick(chosen);
    } else if (event.key === 'Escape') {
      // Лист закрылся бы вместе со списком.
      event.preventDefault();
      event.stopPropagation();
      suggester.clear();
    }
  }
</script>

<div>
  <Field label="Рабочая папка">
    {#snippet children({ id })}<input
      {id}
      bind:this={field}
      aria-label="Рабочая папка"
      aria-autocomplete="list"
      aria-controls={items.length > 0 ? listId : undefined}
      aria-activedescendant={active >= 0 ? optionId(active) : undefined}
      bind:value
      placeholder="/home/user/project"
      autocomplete="off"
      autocapitalize="off"
      spellcheck={false}
      oninput={() => suggester.input(machineId, value)}
      onkeydown={keydown}
      onblur={() => suggester.clear()}
    />{/snippet}
  </Field>
  {#if items.length > 0}
    <div
      id={listId}
      class="list"
      role="listbox"
      aria-label="Подсказки папок"
      tabindex="-1"
      onmousedown={(event) => event.preventDefault()}
      style:top={`${anchor.top}px`}
      style:left={`${anchor.left}px`}
      style:width={`${anchor.width}px`}
      style:max-height={anchor.room > 0 ? `${anchor.room}px` : undefined}
    >
      {#each items as item, index (item)}
        <button id={optionId(index)} type="button" class="item" role="option" tabindex="-1" aria-label={item} aria-selected={index === active} onclick={() => pick(item)}>{nameOf(item)}</button>
      {/each}
    </div>
  {/if}
</div>

<style>
  .list {
    position: fixed;
    z-index: 1000;
    display: flex;
    flex-direction: column;
    gap: var(--s1);
    max-height: calc(var(--target) * 5);
    padding: var(--s2);
    overflow-y: auto;
    border-radius: var(--radius);
    background: var(--raised);
  }
  .item {
    flex: none;
    display: block;
    width: 100%;
    min-height: var(--target);
    border: var(--s0);
    border-radius: var(--radius);
    background: var(--transparent);
    color: var(--secondary);
    padding: var(--s2) var(--s4);
    font-family: var(--font-mono);
    font-size: var(--fs-sm);
    text-align: left;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .item:hover, .item[aria-selected='true'] { background: var(--selected); color: var(--text); }
</style>
