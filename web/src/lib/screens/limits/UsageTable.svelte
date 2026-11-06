<script lang="ts">
  import { resolve } from '$app/paths';
  import { fmt } from '#lib/format/ru.js';
  import { styleProperty } from '#lib/ui/style-property.js';
  import type { usageGroups } from './model.js';
  let { groups }: { groups: ReturnType<typeof usageGroups> } = $props();
</script>
<h2>Сегодня по сессиям</h2>
{#if groups.length}
  <table aria-label="Сегодня по сессиям">
    <thead><tr><th scope="col">Сессия / модель</th><th scope="col">Токены</th><th scope="col">≈ доля недели</th></tr></thead>
    <!-- Группа на провайдера: у каждого своя неделя, итог группы — сколько её ушло сегодня. -->
    {#each groups as group (group.provider)}
      <tbody data-provider={group.provider}>
        <tr class="group"><th scope="rowgroup" colspan="2">{group.label}</th><td class="total">{group.total}</td></tr>
        {#each group.rows as row (row.id)}<tr>
          <td><a href={resolve('/c/[id]', { id: row.conversationId })}>{row.title}</a><small>{row.detail}</small>{#if row.overlapped}<small>Запуски пересекались</small>{/if}</td>
          <td><span class="count" title={`вх. ${fmt.tokens(row.input)}`} aria-hidden="true">вх. {fmt.tokensCompact(row.input)}</span><span class="sr-only">вх. {fmt.tokens(row.input)}</span><span class="count" title={`вых. ${fmt.tokens(row.output)}`} aria-hidden="true">вых. {fmt.tokensCompact(row.output)}</span><span class="sr-only">вых. {fmt.tokens(row.output)}</span></td>
          <td><span class="count">{row.weekly ? fmt.pctApprox(row.delta) : '—'}</span>{#if row.weekly}<span class="share" aria-hidden="true"><span use:styleProperty={{ name: '--share', value: `${row.share}` }}></span></span>{/if}</td>
        </tr>{/each}
      </tbody>
    {/each}
  </table>
  <p class="note">Доли приблизительные: по изменению квоты во время работы, полоска сравнивает строки одного провайдера. Кэш включён во входные токены.</p>
{:else}<p class="empty">Сегодня завершённых запусков ещё нет</p>{/if}
<style>
  table { width: 100%; table-layout: fixed; border-collapse: separate; border-spacing: 0; font-size: var(--fs-meta); }
  thead th { text-align: left; color: var(--muted); font-weight: var(--weight-medium); font-size: var(--fs-sm); padding-block: var(--s0) var(--s4); }
  /* Токены и доля — узкие колонки фиксированной ширины: «вых. 9,88 млрд» и «≈ 100,0%» влезают целиком. */
  thead th:nth-child(2) { width: calc(var(--s4) * 13.5); }
  thead th:last-child { width: calc(var(--s4) * 9.5); text-align: right; }
  /* В широкой таблице заголовок доли помещается в одну строку. */
  @container limits (min-width: 50rem) { thead th:last-child { width: calc(var(--s4) * 13); } }
  td, th { padding: var(--s4) var(--s2); overflow-wrap: anywhere; vertical-align: top; }
  td { font-variant-numeric: tabular-nums; line-height: var(--lh-body); }
  td:last-child { text-align: right; }
  .group th, .group td { padding-block: var(--s8) var(--s3); text-align: left; font-size: var(--fs-meta); font-weight: var(--weight-semibold); }
  .group .total { text-align: right; font-variant-numeric: tabular-nums; white-space: nowrap; }
  tbody tr:not(.group) > td { background: var(--surface); }
  /* Строки группы — одна карточка: скругление на первой и последней строке. */
  tbody tr:nth-child(2) td:first-child { border-start-start-radius: var(--radius-sm); }
  tbody tr:nth-child(2) td:last-child { border-start-end-radius: var(--radius-sm); }
  tbody tr:last-child td:first-child { border-end-start-radius: var(--radius-sm); }
  tbody tr:last-child td:last-child { border-end-end-radius: var(--radius-sm); }
  tbody td:first-child { padding-inline-start: var(--s6); }
  tbody td:last-child { padding-inline-end: var(--s4); }
  small { display: block; color: var(--muted); font-size: var(--fs-sm); }
  /* Число не рвётся на группе разрядов: «129 000» целиком на своей строке. */
  .count { display: block; width: fit-content; max-width: 100%; white-space: nowrap; }
  td:last-child .count { margin-inline-start: auto; }
  /* Полоска сравнивает строки внутри провайдера: самая затратная во всю ширину колонки. */
  .share { display: flex; justify-content: end; margin-top: var(--s2); }
  .share span { width: calc(var(--share) * 100%); min-width: var(--s1); height: var(--s2); background: var(--accent); border-radius: var(--radius-pill); }
  /* Зона нажатия 44px держится отступами: текст ссылки стоит в строку с соседними ячейками. */
  a { --pad: calc((var(--target) - var(--fs-meta) * var(--lh-body)) / 2); display: inline-block; padding-block: var(--pad); margin-block: calc(-1 * var(--pad)); line-height: var(--lh-body); }
  .empty { margin: var(--s0); padding: var(--s8); background: var(--surface); border-radius: var(--radius-card); color: var(--muted); font-size: var(--fs-meta); }
</style>
