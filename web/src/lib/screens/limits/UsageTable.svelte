<script lang="ts">
  import { resolve } from '$app/paths';
  import { fmt } from '#lib/format/ru.js';
  import type { usageRows } from './model.js';
  let { rows }: { rows: ReturnType<typeof usageRows> } = $props();
</script>
<h2>Сегодня по сессиям</h2>
{#if rows.length}
  <table aria-label="Сегодня по сессиям"><thead><tr><th>Сессия / модель</th><th>Токены</th><th>≈ доля недели</th></tr></thead><tbody>
    {#each rows as row (row.id)}<tr><td><a href={resolve('/c/[id]', { id: row.conversationId })}>{row.title}</a><small>{row.detail}</small>{#if row.overlapped}<small>Запуски пересекались</small>{/if}</td><td><span class="count">вх. {fmt.tokens(row.input)}</span><span class="count">вых. {fmt.tokens(row.output)}</span></td><td><span class="count">{row.weekly ? fmt.pctApprox(row.delta) : '—'}</span></td></tr>{/each}
  </tbody></table>
{:else}<p class="note">Сегодня завершённых запусков ещё нет</p>{/if}
<p class="note">Доли по сессиям приблизительные: по изменению квоты во время работы. Кэш включён во входные токены.</p>
<style>
  table { width: 100%; table-layout: fixed; border-collapse: collapse; font-size: var(--fs-meta); }
  th { text-align: left; color: var(--muted); font-weight: var(--weight-medium); }
  th:first-child { width: 45%; }
  td, th { padding: var(--s7) var(--s2); overflow-wrap: anywhere; vertical-align: top; }
  td { font-variant-numeric: tabular-nums; }
  small { display: block; color: var(--muted); }
  /* Число не рвётся на группе разрядов: «129 000» целиком на своей строке. */
  .count { display: block; white-space: nowrap; }
  /* Зона нажатия 44px держится отступами: текст ссылки стоит в строку с соседними ячейками. */
  a { --pad: calc((var(--target) - var(--fs-meta) * var(--lh-body)) / 2); display: inline-block; padding-block: var(--pad); margin-block: calc(-1 * var(--pad)); line-height: var(--lh-body); }
  td { line-height: var(--lh-body); }
</style>
