<script lang="ts">
  import { untrack } from 'svelte';
  import { STATE_TONE, STATE_WORD } from '#lib/screens/plan/planText.js';
  import { arrivedNow } from '#lib/timeline/arrival.js';
  import CardShell from '../CardShell.svelte';
  import type { Tone } from '../copy';
  import { planTaskName, updateTail, type PlanUpdateData } from '../defs/plan';
  import type { CardProps } from '../types';
  import { livePlan, liveTask } from './livePlan';
  import PlanLink from './PlanLink.svelte';
  import PlanStateWord from './PlanStateWord.svelte';

  let { call, data, line, word, env }: CardProps<PlanUpdateData> = $props();
  const plan = livePlan();
  // Новое состояние входит сдвигом только у правки на глазах: прочитанная история стоит сразу.
  const fresh = untrack(() => arrivedNow(`plan-update:${call.block.id}`, call.block.createdAt));
  const running = $derived(call.state === 'running' || !call.output);
  const name = $derived(planTaskName(data));
  const now = $derived(data.error ? null : liveTask(plan, call.conversationId, { id: data.task?.id, number: data.number }));
  // Задача ушла дальше после этой правки: справа её состояние сейчас, тоном экрана плана.
  const after = $derived(data.to ?? data.task?.state ?? null);
  const since = $derived(now && after && now.state !== after ? now.state : null);
  const status = $derived(since ? `сейчас ${STATE_WORD[since]}` : (word?.word ?? ''));
  const findingsKey = $derived(`plan-findings:${call.block.id}`);
  const findingsOpen = $derived(env.folds.isOpen(findingsKey));
  const SHOWN = 2;
  // Находки и заметка видны ниже сами: в строке «Ещё» только то, чего на карточке нет.
  const extra = $derived(data.edits.filter((edit) => edit !== 'findings' && edit !== 'note'));
  const findings = $derived(findingsOpen || data.findings.length <= SHOWN + 1 ? data.findings : data.findings.slice(0, SHOWN));
</script>

<CardShell {call} {env} {line} {status} tone={since ? (STATE_TONE[since] as Tone) : (word?.tone ?? 'muted')} live={word?.live}>
  {#snippet head()}
    {#if data.error}
      <span class="line">{line}</span>
    {:else}
      <span class="name"><PlanLink orchestratorId={call.conversationId} number={data.number}>{name}</PlanLink><span class="colon">:</span></span>
      <span class="move" data-role="plan-update-move">
        {#if data.unchanged || !data.to}
          <span class="tail">{updateTail(data, running)}</span>
        {:else}
          {#if data.from && data.from !== data.to}<PlanStateWord state={data.from} /><span class="arrow">→</span>{/if}<PlanStateWord state={data.to} arriving={fresh} />
        {/if}
      </span>
    {/if}
  {/snippet}
  {#if data.error}<p class="meta danger" data-role="plan-error">{data.error.text}</p>{/if}
  {#if !data.error && data.to && extra.length > 0}<p class="meta" data-role="plan-update-edits">Ещё: {updateTail({ ...data, to: null, edits: extra })}</p>{/if}
  {#if !data.error && data.blocker}<p class="meta warn" data-role="plan-update-blocker">Блокер: {data.blocker}</p>{/if}
  {#if !data.error && data.findings.length > 0}
    <div class="findings" data-role="plan-update-findings">
      <span class="label">Находки</span>
      <ul>
        {#each findings as finding, index (index)}<li>{finding}</li>{/each}
      </ul>
      {#if data.findings.length > findings.length || findingsOpen}
        <button type="button" class="more" aria-expanded={findingsOpen} onclick={() => env.folds.toggle(findingsKey)}>{findingsOpen ? 'Свернуть' : `Все находки · ${data.findings.length}`}</button>
      {/if}
    </div>
  {/if}
  {#if !data.error && data.note && data.edits.includes('note')}<p class="meta note" data-role="plan-update-note">{data.note}</p>{/if}
</CardShell>

<style>
  p { margin: var(--s0); }
  .line { min-width: 0; overflow: hidden; color: var(--text); text-overflow: ellipsis; white-space: nowrap; }
  /* Название сжимается первым: переход состояния остаётся виден целиком. */
  .name { display: flex; flex: 0 1 auto; min-width: 0; color: var(--text); }
  .colon { flex: none; }
  .move { display: inline-flex; flex: none; align-items: baseline; gap: var(--s2); max-width: 60%; }
  .tail { min-width: 0; overflow: hidden; color: var(--secondary); text-overflow: ellipsis; white-space: nowrap; }
  .arrow { color: var(--muted); }
  .meta { color: var(--secondary); overflow-wrap: anywhere; }
  .warn { color: var(--warning); }
  .danger { color: var(--danger); }
  .note { display: -webkit-box; overflow: hidden; -webkit-box-orient: vertical; -webkit-line-clamp: 3; line-clamp: 3; white-space: pre-wrap; }
  .findings { display: flex; flex-direction: column; gap: var(--s1); min-width: 0; }
  .label { color: var(--muted); }
  ul { display: flex; flex-direction: column; gap: var(--s1); margin: var(--s0); padding-inline-start: var(--s6); color: var(--secondary); }
  li { overflow-wrap: anywhere; }
  .more { align-self: flex-start; min-height: var(--target); margin-block: calc(var(--s2) * -1); padding: var(--s0); background: var(--transparent); color: var(--accent-link); }
</style>
