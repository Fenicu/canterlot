<script lang="ts">
  import { resolve } from '$app/paths';
  import { jobHref } from '#lib/jobs/model.js';
  import CardLine from './CardLine.svelte';
  import CardShell from './CardShell.svelte';
  import CodeLines from './CodeLines.svelte';
  import type { JobLogData } from './defs/jobs';
  import type { CardProps } from './types';

  let { call, data, line, link, word, grows, env }: CardProps<JobLogData> = $props();
  const SHOWN = 30;
  const all = $derived(data.text.replace(/\n$/, '').split('\n'));
  const lines = $derived(all.slice(-SHOWN).map((text) => ({ text })));
</script>

<!-- Кусок журнала задачи: раскрытие показывает конец прочитанного, лист задачи — журнал целиком. -->
<CardShell {call} {env} {line} {grows} status={word?.word ?? ''} tone={word?.tone ?? 'muted'} live={word?.live === true} dot={word?.live === true} reveal="журнал">
  {#snippet head()}<CardLine {line} {link} />{/snippet}
  {#if data.expired}<p class="note" data-role="job-log-expired">Полный журнал удалён по сроку, остался хвост</p>{/if}
  {#snippet fold()}
    {#if data.text}
      {#if all.length > lines.length}<p class="note">Последние {lines.length} из {all.length} строк</p>{/if}
      <CodeLines {lines} lang={null} label="Журнал задачи" />
    {:else}
      <p class="note">В этом куске журнала пусто</p>
    {/if}
    {#if data.jobId && data.refusal === null}<a class="open" href={resolve(jobHref(data.jobId) as '/')}>Открыть задачу</a>{/if}
  {/snippet}
</CardShell>

<style>
  .note { margin: var(--s0); color: var(--secondary); font-size: var(--fs-sm); overflow-wrap: anywhere; }
  .open { align-self: flex-start; display: inline-flex; align-items: center; min-height: var(--target); color: var(--accent-link); text-decoration: none; }
  .open:hover, .open:focus-visible { text-decoration: underline; text-underline-offset: var(--s1); }
</style>
