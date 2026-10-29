<script lang="ts">
  import { resolve } from '$app/paths';
  import { fmt } from '#lib/format/ru.js';
  import type { usageRows } from './model.js';
  let { rows }: { rows: ReturnType<typeof usageRows> } = $props();
</script>
<h2>Сегодня по сессиям</h2>
<table aria-label="Сегодня по сессиям"><thead><tr><th>Сессия / модель</th><th>Токены, вх. / вых.</th><th>≈ доля недели</th></tr></thead><tbody>
  {#each rows as row (row.id)}<tr><td><a href={resolve('/c/[id]', { id: row.conversationId })}>{row.title}</a><small>{row.detail}</small>{#if row.approximate}<small>приблизительно</small>{/if}{#if row.overlapped}<small>Запуски пересекались</small>{/if}</td><td>{fmt.tokens(row.input)} / {fmt.tokens(row.output)}</td><td>{fmt.pctApprox(row.delta)}</td></tr>{/each}
</tbody></table>
{#if !rows.length}<p class="note">Сегодня завершённых запусков ещё нет</p>{/if}
<p class="note">Доли по сессиям приблизительные: по изменению квоты во время работы. Кэш включён во входные токены.</p>
<style>
  table { width: 100%; table-layout: fixed; border-collapse: collapse; font-size: var(--fs-meta); }
  th { text-align: left; color: var(--muted); font-weight: var(--weight-medium); }
  th:first-child { width: 45%; }
  td, th { padding: var(--s7) var(--s2); overflow-wrap: anywhere; vertical-align: top; }
  td { font-variant-numeric: tabular-nums; }
  small { display: block; color: var(--muted); }
</style>
