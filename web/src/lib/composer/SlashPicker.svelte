<script lang="ts">
  import type { SlashItem } from '#lib/composer/slash.js';

  let {
    items,
    active,
    query,
    anchor,
    listId,
    onpick,
    onclose
  }: {
    items: SlashItem[];
    active: number;
    query: string;
    anchor: HTMLElement | null;
    listId: string;
    onpick: (item: SlashItem) => void;
    onclose: () => void;
  } = $props();

  let place_box = $state({ left: 8, bottom: 8, width: 320, room: 320 });

  function place() {
    if (!anchor || typeof window === 'undefined') return;
    const box = anchor.getBoundingClientRect();
    const edge = 8;
    const gap = 4;
    const width = Math.min(Math.max(box.width, 240), window.innerWidth - edge * 2);
    const left = Math.min(Math.max(box.left, edge), Math.max(edge, window.innerWidth - width - edge));
    place_box = {
      left,
      bottom: window.innerHeight - box.top + gap,
      width,
      room: Math.max(120, box.top - gap - edge)
    };
  }

  const optionId = (index: number) => `${listId}-${index}`;

  $effect(() => {
    if (items.length === 0) return;
    place();
    const update = () => place();
    window.addEventListener('resize', update);
    window.addEventListener('scroll', update, true);
    const outside = (event: PointerEvent) => {
      const target = event.target as Node | null;
      if (!target) return;
      if (anchor?.contains(target)) return;
      if (document.getElementById(listId)?.contains(target)) return;
      onclose();
    };
    document.addEventListener('pointerdown', outside, true);
    return () => {
      window.removeEventListener('resize', update);
      window.removeEventListener('scroll', update, true);
      document.removeEventListener('pointerdown', outside, true);
    };
  });
</script>

{#if items.length > 0}
  <div
    id={listId}
    class="picker"
    role="listbox"
    aria-label="Команды и навыки"
    tabindex="-1"
    onmousedown={(event) => event.preventDefault()}
    style:left={`${place_box.left}px`}
    style:bottom={`${place_box.bottom}px`}
    style:width={`${place_box.width}px`}
    style:max-height={`${place_box.room}px`}
  >
    <p class="caption">Команды и навыки{query ? ` · ${query}` : ''}</p>
    {#each items as item, index (item.name + item.kind)}
      <button
        id={optionId(index)}
        type="button"
        class="item"
        class:selected={index === active}
        role="option"
        tabindex="-1"
        aria-selected={index === active}
        onmousedown={(event) => event.preventDefault()}
        onclick={() => onpick(item)}
      >
        <span class="line">
          <span class="name">/{item.name}</span>
          {#if item.argument_hint}<span class="hint">{item.argument_hint}</span>{/if}
          <span class="kind">{item.kind === 'skill' ? 'навык' : 'команда'}</span>
        </span>
        {#if item.description}<span class="desc">{item.description}</span>{/if}
      </button>
    {/each}
  </div>
{/if}

<style>
  .picker {
    position: fixed;
    z-index: 1000;
    display: flex;
    flex-direction: column;
    gap: var(--s1);
    padding: var(--s2);
    overflow-x: hidden;
    overflow-y: auto;
    overscroll-behavior: contain;
    border-radius: var(--radius);
    background: var(--raised);
  }
  .caption {
    margin: 0;
    padding: var(--s1) var(--s2);
    color: var(--muted);
    font-size: var(--fs-meta);
  }
  .item {
    flex: none;
    display: block;
    width: 100%;
    min-height: var(--target);
    padding: var(--s2) var(--s3);
    border: var(--s0);
    border-radius: var(--radius);
    background: var(--transparent);
    color: var(--text);
    text-align: left;
  }
  .item:hover, .item.selected { background: var(--selected); }
  .line { display: flex; align-items: baseline; gap: var(--s2); min-width: 0; }
  .name { color: var(--text); font-family: var(--font-mono); font-size: var(--fs-sm); }
  .hint { color: var(--secondary); font-size: var(--fs-meta); }
  .kind { margin-inline-start: auto; color: var(--muted); font-size: var(--fs-meta); white-space: nowrap; }
  .desc {
    display: block;
    margin-top: var(--s1);
    color: var(--muted);
    font-size: var(--fs-sm);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>
