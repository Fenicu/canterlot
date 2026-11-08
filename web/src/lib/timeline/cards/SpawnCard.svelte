<script lang="ts">
  import { getRuntime } from '#lib/app/runtime.js';
  import { asDecision, catalogTitles, routeLine } from '#lib/routing/decision.js';
  import { routingTitles } from '#lib/routing/titles.svelte.js';
  import CardLine from './CardLine.svelte';
  import CardShell from './CardShell.svelte';
  import { firstLine } from './copy';
  import { placeLine, queueLine, spawnExecutor, spawnWord, type SpawnData } from './defs/sessions';
  import { planTaskName } from './defs/plan';
  import { livePlan, liveTask } from './plan/livePlan';
  import PlanLink from './plan/PlanLink.svelte';
  import PlanStateWord from './plan/PlanStateWord.svelte';
  import type { CardProps } from './types';

  let { call, data, line, link, grows, env }: CardProps<SpawnData> = $props();

  let runtime: ReturnType<typeof getRuntime> | null = null;
  try {
    runtime = getRuntime();
  } catch {
    runtime = null;
  }

  // Состояние ребёнка живое: строка списка сессий меняется событием хаба.
  const word = $derived(spawnWord(call, data, env.now));
  const routed = $derived(data.status === 'started' && data.routing ? data.routing : null);
  $effect(() => {
    if (routed) routingTitles.ensure(runtime?.list.routingRevision ?? null);
  });
  // Полное решение с пропусками хранит строка ребёнка; в ответе инструмента только выбор.
  const route = $derived.by(() => {
    if (!routed) return '';
    const row = data.sessionId ? runtime?.list.items.get(data.sessionId) : undefined;
    const decision = asDecision(row?.routing) ?? asDecision({ considered: [], ...routed });
    if (!decision) return '';
    const titles = catalogTitles(runtime?.machines.items.values() ?? []);
    return routeLine(decision, routingTitles.title(decision.task_type), titles);
  });
  // Название модели по каталогу машин, как в строке маршрута; без каталога — по идентификатору.
  const executor = $derived(spawnExecutor(data.harness, data.model, data.effort, catalogTitles(runtime?.machines.items.values() ?? [])));
  const meta = $derived([route ? '' : executor, placeLine(data)].filter(Boolean).join(' · '));
  const queued = $derived(queueLine(data, env.now));
  const briefKey = $derived(`brief:${call.block.id}`);
  const briefOpen = $derived(env.folds.isOpen(briefKey));
  const lead = $derived(firstLine(data.brief));
  const more = $derived(data.brief.trim() !== lead || data.briefCut);
  // Задача плана живая: делегат закончил — «на проверке», приёмка — «готова».
  const plan = livePlan();
  const planTask = $derived(data.planTask ? liveTask(plan, call.conversationId, { number: data.planTask.number }) : null);
  const planName = $derived(data.planTask ? planTaskName({ number: data.planTask.number, title: planTask?.title ?? call.planBefore(data.planTask.number)?.title ?? '' }) : '');
</script>

<CardShell {call} {env} {line} {grows} status={word.word} tone={word.tone} dot={data.status === 'started' || data.status === 'running'} live={word.live}>
  {#snippet head()}<CardLine {line} {link} />{/snippet}
  {#if route}<p class="meta" data-role="spawn-route-line">{route}</p>{/if}
  {#if meta}<p class="meta" data-role="spawn-meta">{meta}</p>{/if}
  {#if queued}<p class="meta" data-role="spawn-queued-line">{queued}</p>{/if}
  {#if data.planTask && data.status !== 'error'}
    <p class="meta plan" data-role="spawn-plan-task">
      {#if data.planTask.linked === false}
        <span>задача {planName} не связана: её закрыли или перенесли</span>
      {:else}
        <span class="plan-label">задача</span><PlanLink orchestratorId={call.conversationId} number={data.planTask.number}>{planName}</PlanLink>{#if planTask && data.status === 'started'}<PlanStateWord state={planTask.state} />{/if}
      {/if}
    </p>
  {/if}
  {#if data.status === 'pending_approval' && !data.decision}<p class="meta warn" data-role="spawn-pending">Исполнитель вне списка владельца: решение придёт сообщением</p>{/if}
  {#if data.error}<p class="meta danger" data-role="spawn-error">{data.error}</p>{/if}
  {#if data.metricsStale}<p class="meta" data-role="spawn-metrics-stale">Метрики машины устарели: страж не проверял запас</p>{/if}
  {#if lead}
    {#if more}
      <button type="button" class="brief" aria-expanded={briefOpen} data-role="spawn-brief" onclick={() => env.folds.toggle(briefKey)}>
        <span class="lead">{briefOpen ? 'Бриф' : lead}</span><span class="toggle">{briefOpen ? 'Свернуть' : 'Весь бриф'}</span>
      </button>
      {#if briefOpen}
        <p class="full" data-role="spawn-brief-full">{data.brief}{#if data.briefCut}…{/if}</p>
        {#if data.briefCut}<p class="meta">В истории лежит начало брифа: целиком он в сыром вызове</p>{/if}
      {/if}
    {:else}
      <p class="meta brief-line" data-role="spawn-brief">{lead}</p>
    {/if}
  {/if}
</CardShell>

<style>
  p { margin: var(--s0); }
  .meta { color: var(--secondary); overflow-wrap: anywhere; }
  .warn { color: var(--warning); }
  .plan { display: flex; align-items: baseline; gap: var(--s3); min-width: 0; }
  .plan-label { flex: none; }
  .danger { color: var(--danger); }
  .brief-line { overflow: hidden; color: var(--muted); text-overflow: ellipsis; white-space: nowrap; }
  .brief { display: flex; align-items: center; gap: var(--s4); width: 100%; min-width: 0; min-height: var(--target); margin-block: calc(var(--s2) * -1); padding: var(--s0); background: var(--transparent); color: var(--muted); text-align: left; }
  .brief .lead { flex: 1 1 auto; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .brief .toggle { flex: none; color: var(--accent-link); }
  .full { color: var(--text); overflow-wrap: anywhere; white-space: pre-wrap; }
</style>
