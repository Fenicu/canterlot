<script lang="ts">
  import type { Snippet } from 'svelte';
  import { resolve } from '$app/paths';
  import { feedCards } from '../context';
  import { planHref } from './livePlan';

  let {
    orchestratorId,
    number = null,
    inline = false,
    children
  }: {
    orchestratorId: string;
    number?: number | null;
    /** Ссылка стоит внутри фразы: её отличает подчёркивание, а не только цвет. */
    inline?: boolean;
    children: Snippet;
  } = $props();
  const feed = feedCards();

  // В своём чате план открывается вкладкой панели делегирования, как с полосы плана; иначе — страница плана.
  function open(event: MouseEvent): void {
    if (event.defaultPrevented || event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
    if (feed.openPlan?.(orchestratorId)) event.preventDefault();
  }
</script>

{#if orchestratorId}
  <a class="plan-link" class:inline href={resolve(planHref(orchestratorId) as '/')} data-plan-number={number ?? undefined} onclick={open}>{@render children()}</a>
{:else}
  <span class="plan-link">{@render children()}</span>
{/if}

<style>
  .plan-link { min-width: 0; overflow: hidden; color: var(--accent-link); text-decoration: none; text-overflow: ellipsis; white-space: nowrap; }
  /* Ссылка в строке текста: подчёркивание вместо одного цвета, как у ссылок prose. */
  .plan-link.inline { text-decoration: underline; text-underline-offset: var(--s1); }
  span.plan-link { color: var(--text); text-decoration: none; }
  a:hover, a:focus-visible { text-decoration: underline; text-underline-offset: var(--s1); }
</style>
