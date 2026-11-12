<script lang="ts">
  import type { Tab } from './grouping';

  let {
    tabs,
    current,
    counted = true,
    onselect
  }: {
    tabs: { tab: Tab; label: string; count: number }[];
    current: Tab;
    /** Счёт известен только после снимка: до него нули выглядели бы как пустой список. */
    counted?: boolean;
    onselect: (tab: Tab) => void;
  } = $props();

  function same(a: Tab, b: Tab): boolean {
    if (a.kind !== b.kind) return false;
    return a.kind !== 'group' || b.kind !== 'group' || a.id === b.id;
  }

  // Пустой фильтр не занимает места; «Все» и выбранный видны всегда, до снимка счёт неизвестен.
  const shown = $derived(tabs.filter((item) => !counted || item.tab.kind === 'all' || item.count > 0 || same(item.tab, current)));

  let strip = $state<HTMLElement | null>(null);
  let fadeStart = $state(false);
  let fadeEnd = $state(false);

  // Край гаснет только там, где за ним есть ещё вкладки.
  function measure() {
    if (!strip) return;
    fadeStart = strip.scrollLeft > 1;
    fadeEnd = strip.scrollLeft + strip.clientWidth < strip.scrollWidth - 1;
  }

  // Выбранная вкладка не прячется под гаснущим краем.
  $effect(() => {
    void current;
    void shown.map((item) => item.label + item.count).join();
    const node = strip;
    if (!node) return;
    const tab = node.querySelector<HTMLElement>('[aria-selected="true"]');
    if (tab && node.scrollWidth > node.clientWidth) {
      const pad = parseFloat(getComputedStyle(node).scrollPaddingInlineStart) || 0;
      const box = node.getBoundingClientRect();
      const own = tab.getBoundingClientRect();
      if (own.left < box.left + pad) node.scrollLeft -= box.left + pad - own.left;
      else if (own.right > box.right - pad) node.scrollLeft += own.right - (box.right - pad);
    }
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(node);
    for (const child of node.children) observer.observe(child);
    return () => observer.disconnect();
  });

  function move(event: KeyboardEvent) {
    const forward = event.key === 'ArrowRight' || event.key === 'ArrowDown';
    const back = event.key === 'ArrowLeft' || event.key === 'ArrowUp';
    if (!forward && !back) return;
    event.preventDefault();
    const index = shown.findIndex((item) => same(item.tab, current));
    const next = shown[(index + (forward ? 1 : -1) + shown.length) % shown.length];
    if (next) onselect(next.tab);
  }
</script>

<div bind:this={strip} class="tabs" data-top-chrome data-fine-targets data-fade-start={fadeStart ? '' : undefined} data-fade-end={fadeEnd ? '' : undefined} role="tablist" aria-label="Группы сессий" tabindex="-1" onkeydown={move} onscroll={measure}>
  {#each shown as item, index (item.label + index)}
    <button
      type="button"
      role="tab"
      aria-selected={same(item.tab, current)}
      tabindex={same(item.tab, current) ? 0 : -1}
      onclick={() => onselect(item.tab)}
    >
      {item.label} {#if counted}<span class="count">{item.count}</span>{/if}
    </button>
  {/each}
</div>

<style>
  .tabs {
    display: flex;
    gap: var(--s2);
    min-width: 0;
    overflow-x: auto;
    scrollbar-width: none;
    margin: var(--s3) var(--gutter);
    padding-block: var(--s2);
  }
  .tabs { scroll-padding-inline: var(--s9); }
  [data-fade-end] { mask-image: linear-gradient(to left, var(--transparent), var(--text) var(--s9)); }
  [data-fade-start] { mask-image: linear-gradient(to right, var(--transparent), var(--text) var(--s9)); }
  [data-fade-start][data-fade-end] { mask-image: linear-gradient(to right, var(--transparent), var(--text) var(--s9), var(--text) calc(100% - var(--s9)), var(--transparent)); }
  :global([data-pane='list']) .tabs { mask-image: none; }
  button {
    flex: 1 0 auto;
    min-height: var(--target);
    border: var(--s0);
    border-radius: var(--radius-pill);
    background: var(--transparent);
    color: var(--muted);
    font-size: var(--fs-meta);
    padding: var(--s4) var(--s6);
    white-space: nowrap;
  }
  .count { display: inline-block; min-width: var(--s9); margin-left: var(--s1); padding-inline: var(--s3); border-radius: var(--radius-pill); background: var(--chip); color: var(--chip-text); font-size: var(--fs-sm); }
  /* Выбор заметен в обеих темах: заливка акцентом, как у нижней навигации. */
  button[aria-selected='true'] { background: var(--accent); color: var(--accent-ink); font-weight: var(--weight-medium); }
  button[aria-selected='true'] .count { background: var(--accent-ink); color: var(--accent); }
  :global([data-pane='list']) .tabs { flex-wrap: wrap; overflow-x: visible; }
  :global([data-pane='list']) button { flex: 0 1 auto; min-width: 0; max-width: 100%; padding-inline: var(--s4); overflow-wrap: anywhere; white-space: normal; }
  /* Точный указатель не нуждается в пальцевой мишени: чипы мелкие и плотные. */
  @media (pointer: fine) {
    .tabs { gap: var(--s3); margin-block: var(--s2); padding-block: var(--s1); }
    button { flex: 0 0 auto; min-height: var(--s11); padding: var(--s1) var(--s5); }
    :global([data-pane='list']) button { padding-inline: var(--s5); }
  }
</style>
