<script lang="ts">
  import { kidsTitle, type TreeNode } from './grouping';
  import Icon from '#lib/ui/Icon.svelte';
  import { rowFold } from '#lib/ui/fold.js';
  import SessionRow from './SessionRow.svelte';

  let {
    id,
    title,
    count,
    items,
    collapsed,
    now,
    current = '',
    heading = true,
    copy = false,
    keyPrefix = '',
    headingLabel = '',
    tone = null,
    ontoggle,
    ontoggleKids,
    onopen
  }: {
    id: string;
    title: string;
    count: number;
    items: TreeNode[];
    collapsed: boolean;
    now: number;
    current?: string;
    heading?: boolean;
    /** Копия строки в сводном разделе: у неё свой ключ и своя метка, без `data-session`. */
    copy?: boolean;
    /** Префикс ключей копий: тот же id в своей группе остаётся отдельной строкой. */
    keyPrefix?: string;
    /** Имя заголовка целиком, если счёт читается вместе с ним: «Ждут вас · 2». */
    headingLabel?: string;
    tone?: 'warning' | null;
    ontoggle: (id: string) => void;
    ontoggleKids: (id: string) => void;
    onopen: (id: string, node?: HTMLElement | null) => void;
  } = $props();
</script>

{#snippet branch(node: TreeNode)}
  {#if node.children.length > 0}
    {@const label = node.open
      ? `Свернуть делегированные: ${node.children.length}`
      : `Развернуть делегированные: ${node.children.length}`}
    <div class="kids-list" class:nested={node.depth > 0}>
      <button
        type="button"
        class="kids depth-{Math.min(node.depth + 1, 2)}"
        aria-expanded={node.open}
        aria-label={label}
        onclick={() => ontoggleKids(node.item.id)}
      >
        <span class="kids-title">{kidsTitle(node)}</span>
        <span class="chevron" class:open={node.open}><Icon name="chevron" /></span>
      </button>
      {#if node.open}
        {#each node.children as child (child.item.id)}
          <div class="slot" data-slot={child.item.id} transition:rowFold>
            <SessionRow item={child.item} {now} depth={child.depth} current={child.item.id === current} {onopen} />
            {@render branch(child)}
          </div>
        {/each}
      {/if}
    </div>
  {/if}
{/snippet}

<section>
  {#if heading}
    <h2>
      <button type="button" class="section-head" aria-expanded={!collapsed} aria-label={headingLabel || undefined} onclick={() => ontoggle(id)}>
        <span class="title" data-tone={tone}>{title}</span>
        <span class="chevron" class:open={!collapsed}><Icon name="chevron" /></span>
        <span class="count" data-tone={tone}>{count}</span>
      </button>
    </h2>
  {/if}
  {#if !heading || !collapsed}
    {#each items as node (`${keyPrefix}${node.item.id}`)}
      <!-- Строка, ушедшая в архив или удалённая, схлопывается; вернувшаяся раскрывается на своём месте. -->
      <div class="slot" data-slot={`${keyPrefix}${node.item.id}`} transition:rowFold>
        <SessionRow item={node.item} {now} depth={node.depth} current={!copy && node.item.id === current} {copy} {onopen} />
        {@render branch(node)}
      </div>
    {/each}
  {/if}
</section>

<style>
  h2 { margin: 0; font-size: var(--fs-sm); font-weight: var(--weight-medium); }
  button {
    display: flex;
    align-items: center;
    gap: var(--s2);
    width: 100%;
    text-align: left;
    border: var(--s0);
    background: var(--transparent);
    color: var(--time);
  }
  /* Тонкий заголовок группы: подпись заглавными и счёт справа, стрелка видна у свёрнутой. */
  .section-head { min-height: var(--target); padding: var(--s3) var(--s7) var(--s1); text-transform: uppercase; }
  .chevron { flex: none; display: inline-flex; color: var(--muted); transition: transform var(--duration-fast) var(--ease); }
  .chevron :global(svg) { width: var(--icon-small); height: var(--icon-small); }
  .chevron.open { transform: rotate(90deg); }
  .section-head .chevron.open { opacity: 0; }
  .section-head:hover .chevron.open, .section-head:focus-visible .chevron.open { opacity: 1; }
  .title { flex: 0 1 auto; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  /* Раздел ожидания красит подпись янтарём; слово «Ждут вас» читается и без цвета. */
  .title[data-tone='warning'] { color: var(--warning); }
  .count { flex: none; margin-left: auto; color: var(--time); font-variant-numeric: tabular-nums; }
  .count[data-tone='warning'] { color: var(--warning); font-weight: var(--weight-bold); }
  @media (pointer: fine) {
    .section-head { min-height: var(--s11); }
  }
  /* Делегированные под родителем: линия слева, компактные строки. */
  .kids-list { position: relative; margin: var(--s0) var(--s3) var(--s1) calc(var(--s3) + var(--s5) + var(--avatar) / 2); padding-left: calc(var(--s2) + var(--hairline) * 2); }
  .kids-list::before { content: ''; position: absolute; inset: var(--s0) auto var(--s0) var(--s0); width: calc(var(--hairline) * 2); border-radius: var(--radius-pill); background: var(--line); }
  .kids-list.nested { margin-left: calc(var(--s5) + var(--avatar-small) / 2); }
  .kids-list :global(.row) { margin-inline: var(--s0); }
  .kids {
    min-height: var(--s11);
    padding: var(--s0) var(--s3);
    color: var(--time);
    font-size: var(--fs-sm);
  }
  .kids-title { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  @media (pointer: coarse) { .kids { min-height: var(--target); } }
</style>
