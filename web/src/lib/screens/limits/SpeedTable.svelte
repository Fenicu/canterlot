<script lang="ts">
  import type { speedRows } from './model.js';
  let { rows }: { rows: ReturnType<typeof speedRows> } = $props();
</script>
<h2>Скорость моделей</h2>
<table aria-label="Скорость моделей"><thead><tr><th>Модель</th><th>Сегодня</th><th>За неделю</th></tr></thead><tbody>
  {#each rows as row (row.id)}<tr>
    <td>{row.model}<small>{row.provider}</small></td>
    {#each [row.today, row.period] as cell, index (index)}<td>{#if cell.speed || cell.firstToken}{#if cell.speed}<span class="num">{cell.speed}</span>{/if}{#if cell.firstToken}<small class="num">{cell.firstToken}</small>{/if}{:else}<span class="none">нет ходов</span>{/if}</td>{/each}
  </tr>{/each}
</tbody></table>
{#if !rows.length}<p class="note">За неделю ещё нет ходов с измеренной скоростью</p>{/if}
<p class="note">Медиана по ходам. Генерация считается без работы инструментов и ожиданий, «старт» — время до первого токена от начала запроса к модели. Codex не передаёт время рассуждений, его скорость не считается.</p>
<style>
  table { width: 100%; table-layout: fixed; border-collapse: collapse; font-size: var(--fs-meta); }
  th { text-align: left; color: var(--muted); font-weight: var(--weight-medium); }
  th:first-child { width: 40%; }
  td, th { padding: var(--s7) var(--s2); overflow-wrap: anywhere; vertical-align: top; }
  td { font-variant-numeric: tabular-nums; }
  small, .none { display: block; color: var(--muted); }
  /* Число с единицей не рвётся: «0,7 с» и «1 240 ток/с» целиком. */
  .num { display: block; white-space: nowrap; }
</style>
