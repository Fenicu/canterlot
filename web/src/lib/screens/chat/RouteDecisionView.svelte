<script lang="ts">
  import { chosenText, effortName, pairName, type ModelTitles, type RouteDecision } from '#lib/routing/decision.js';
  import { reasonText, verdictLabel } from '#lib/routing/reasons.js';

  let { decision, titles }: { decision: RouteDecision; titles?: ModelTitles } = $props();

  function nudgeText(value: unknown): string {
    if (value === true) return 'включён';
    if (value && typeof value === 'object') {
      const max = (value as { max_nudges?: unknown }).max_nudges;
      return typeof max === 'number' ? `до ${max} толчков` : 'включён';
    }
    return 'выключен';
  }
</script>

<div class="decision" data-role="route-decision">
  <p class="chosen"><span class="label">Выбрано</span> <strong data-role="route-chosen">{chosenText(decision, titles)}</strong></p>
  <dl class="facts">
    {#if decision.level}<div><dt>уровень</dt><dd>{effortName(decision.level)}</dd></div>{/if}
    <div><dt>автопинок</dt><dd>{nudgeText(decision.nudge)}</dd></div>
    {#if decision.watchdog_minutes}<div><dt>сторож</dt><dd>{decision.watchdog_minutes} мин</dd></div>{/if}
    {#if decision.preset}<div><dt>пресет</dt><dd>{decision.preset}</dd></div>{/if}
    {#if decision.policy_revision !== undefined}<div><dt>ревизия политики</dt><dd>{decision.policy_revision}</dd></div>{/if}
  </dl>
  {#if decision.considered.length > 0}
    <h3>Кандидаты по порядку</h3>
    <ol class="considered">
      {#each decision.considered as entry, index (index)}
        <li data-verdict={entry.verdict}>
          <span class="pair">{pairName(entry.harness, entry.model, titles)}</span>
          <span class="verdict">{verdictLabel(entry.verdict)}</span>
          {#if entry.reason}<span class="why">{reasonText(entry.reason)}</span>{/if}
        </li>
      {/each}
    </ol>
  {/if}
</div>

<style>
  .decision { display: flex; flex-direction: column; gap: var(--s4); }
  p { margin: var(--s0); }
  .label { color: var(--secondary); }
  strong { font-weight: var(--weight-semibold); }
  .facts { display: grid; grid-template-columns: max-content minmax(0, 1fr); align-items: baseline; gap: var(--s2) var(--s6); margin: var(--s0); }
  .facts div { display: contents; }
  dt { color: var(--secondary); font-size: var(--fs-sm); }
  dd { margin: var(--s0); font-size: var(--fs-sm); }
  h3 { margin: var(--s2) var(--s0) var(--s0); font-size: var(--fs-meta); font-weight: var(--weight-medium); }
  .considered { display: flex; flex-direction: column; gap: var(--s2); margin: var(--s0); padding-left: var(--s8); }
  li { display: flex; flex-wrap: wrap; align-items: baseline; gap: var(--s1) var(--s4); font-size: var(--fs-meta); }
  .pair { color: var(--text); }
  .verdict { color: var(--secondary); font-size: var(--fs-sm); }
  li[data-verdict='chosen'] .verdict { color: var(--success); }
  .why { flex-basis: 100%; color: var(--secondary); font-size: var(--fs-sm); overflow-wrap: anywhere; }
</style>
