<script lang="ts">
  import { resolve } from '$app/paths';
  import { fmt } from '#lib/format/ru.js';
  import type { usageRows } from './model.js';
  let { rows }: { rows: ReturnType<typeof usageRows> } = $props();
</script>
<h2>Сегодня по сессиям</h2>
{#if rows.length}
  <table aria-label="Сегодня по сессиям"><thead><tr><th>Сессия / модель</th><th>Токены</th><th>≈ доля недели</th></tr></thead><tbody>
    {#each rows as row (row.id)}<tr><td><a href={resolve('/c/[id]', { id: row.conversationId })}>{row.title}</a><small>{row.detail}</small>{#if row.overlapped}<small>Запуски пересекались</small>{/if}</td><td><span class="count" title={`вх. ${fmt.tokens(row.input)}`} aria-hidden="true">вх. {fmt.tokensCompact(row.input)}</span><span class="sr-only">вх. {fmt.tokens(row.input)}</span><span class="count" title={`вых. ${fmt.tokens(row.output)}`} aria-hidden="true">вых. {fmt.tokensCompact(row.output)}</span><span class="sr-only">вых. {fmt.tokens(row.output)}</span></td><td><span class="count">{row.weekly ? fmt.pctApprox(row.delta) : '—'}</span></td></tr>{/each}
  </tbody></table>
{:else}<p class="note">Сегодня завершённых запусков ещё нет</p>{/if}
<p class="note">Доли по сессиям приблизительные: по изменению квоты во время работы. Кэш включён во входные токены.</p>
<style>
  table { width: 100%; table-layout: fixed; border-collapse: collapse; font-size: var(--fs-meta); }
  th { text-align: left; color: var(--muted); font-weight: var(--weight-medium); }
  /* Доля недели — фиксированная узкая колонка: «≈ 100,0%» и «—» влезают целиком. */
  th:first-child { width: 40%; }
  th:last-child { width: 9ch; }
  td, th { padding: var(--s7) var(--s2); overflow-wrap: anywhere; vertical-align: top; }
  td { font-variant-numeric: tabular-nums; }
  small { display: block; color: var(--muted); }
  /* Число не рвётся на группе разрядов: «129 000» целиком на своей строке. */
  .count { display: block; width: fit-content; max-width: 100%; white-space: nowrap; }
  /* Зона нажатия 44px держится отступами: текст ссылки стоит в строку с соседними ячейками. */
  a { --pad: calc((var(--target) - var(--fs-meta) * var(--lh-body)) / 2); display: inline-block; padding-block: var(--pad); margin-block: calc(-1 * var(--pad)); line-height: var(--lh-body); }
  td { line-height: var(--lh-body); }
</style>
