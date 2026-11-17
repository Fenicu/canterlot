<script lang="ts">
  import CardShell from './CardShell.svelte';
  import type { SkillData } from './defs/tools';
  import type { CardProps } from './types';

  let { call, data, line, word, grows, quiet, env }: CardProps<SkillData> = $props();
  const running = $derived(call.state === 'running');
</script>

<!--
  Навык — отдельная строка с именем крупно. Пока навык грузится, по имени проходит блик тона
  акцента и возвращается; готовый навык стоит спокойно. Без движения — только слово «идёт».
-->
<CardShell {call} {env} {line} {grows} {quiet} status={word?.word ?? ''} tone={word?.tone ?? 'muted'}>
  {#snippet head()}
    <span class="kind">Навык</span>
    <span class="name" data-role="skill-name" data-name={data.name} data-glint={running ? '' : undefined}>{data.name}</span>
    {#if data.plugin}<span class="plugin" data-role="skill-plugin">{data.plugin}</span>{/if}
  {/snippet}
  {#if data.args}<p class="args" data-role="skill-args">{data.args}</p>{/if}
</CardShell>

<style>
  .kind { flex: none; color: var(--secondary); }
  .name { position: relative; min-width: 0; overflow: hidden; color: var(--text); font-weight: var(--weight-semibold); text-overflow: ellipsis; white-space: nowrap; transition: color var(--duration-fast) var(--ease); }
  /* Копия имени (псевдоэлемент с пустым текстом для чтения с экрана: имя не звучит дважды) в тоне акцента, срезанная до полосы: полоса проходит слева направо и уходит. */
  .name::after { content: attr(data-name); content: attr(data-name) / ''; position: absolute; inset: var(--s0); overflow: hidden; color: var(--accent-link); text-overflow: ellipsis; white-space: nowrap; clip-path: inset(var(--s0) 100% var(--s0) var(--s0)); pointer-events: none; user-select: none; }
  .name[data-glint]::after { animation: skill-glint var(--duration-stage-pulse) var(--ease-stage-pulse) infinite; }
  @keyframes skill-glint {
    0% { clip-path: inset(var(--s0) 100% var(--s0) var(--s0)); }
    55% { clip-path: inset(var(--s0) var(--s0) var(--s0) var(--s0)); }
    100% { clip-path: inset(var(--s0) var(--s0) var(--s0) 100%); }
  }
  .plugin { min-width: 0; overflow: hidden; color: var(--muted); text-overflow: ellipsis; white-space: nowrap; }
  .args { margin: var(--s0); color: var(--secondary); overflow-wrap: anywhere; display: -webkit-box; -webkit-box-orient: vertical; -webkit-line-clamp: 3; line-clamp: 3; overflow: hidden; }
  @media (prefers-reduced-motion: reduce) {
    .name[data-glint]::after { animation: none; }
  }
</style>
