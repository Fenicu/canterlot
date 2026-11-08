<script lang="ts">
  import { resolve } from '$app/paths';
  import { splitLink } from './links';
  import PlanLink from './plan/PlanLink.svelte';
  import type { CardLink } from './types';

  let { line, link = null }: { line: string; link?: CardLink | null } = $props();
  const parts = $derived(splitLink(line, link));
</script>

<!-- Строка карточки: часть со ссылкой ведёт в сессию или на страницу, остальное — текст. -->
<!-- Внешний адрес страницы или Taiga: маршрутизатор в нём не участвует. -->
<!-- eslint-disable svelte/no-navigation-without-resolve -->
<span class="line">{parts.before}{#if parts.link}{#if parts.link.plan}<PlanLink orchestratorId={parts.link.plan}>{parts.link.text}</PlanLink>{:else if parts.link.external}<a href={parts.link.href} target="_blank" rel="noopener noreferrer">{parts.link.text}</a>{:else}<a href={resolve(parts.link.href as '/')}>{parts.link.text}</a>{/if}{/if}{parts.after}</span>
<!-- eslint-enable svelte/no-navigation-without-resolve -->

<style>
  .line { min-width: 0; overflow: hidden; color: var(--text); text-overflow: ellipsis; white-space: nowrap; }
  :global([data-quiet]) .line { color: var(--muted); }
  a { color: var(--accent-link); text-decoration: none; }
  a:hover, a:focus-visible { text-decoration: underline; text-underline-offset: var(--s1); }
</style>
