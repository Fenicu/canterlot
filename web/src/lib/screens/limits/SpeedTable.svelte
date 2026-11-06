<script lang="ts">
  import type { speedRows } from './model.js';
  let { rows }: { rows: ReturnType<typeof speedRows> } = $props();
</script>
<h2>Скорость моделей</h2>
{#if rows.length}
  <table aria-label="Скорость моделей"><thead><tr><th scope="col">Модель</th><th scope="col">Сегодня</th><th scope="col">За неделю</th></tr></thead><tbody>
    {#each rows as row (row.id)}<tr>
      <th scope="row">{row.model}<small>{row.provider}</small></th>
      {#each [row.today, row.period] as cell, index (index)}<td>{#if cell.speed || cell.firstToken}{#if cell.speed}<span class="num">{cell.speed}</span>{/if}{#if cell.firstToken}<small class="num">{cell.firstToken}</small>{/if}{:else}<span class="none">нет ходов</span>{/if}</td>{/each}
    </tr>{/each}
  </tbody></table>
{:else}<p class="empty">За неделю ещё нет ходов с измеренной скоростью</p>{/if}
<p class="note">Медиана по ходам. Генерация считается без работы инструментов и ожиданий, «старт» — время до первого токена от начала запроса к модели. Codex не передаёт время рассуждений, его скорость не считается.</p>
<style>
  table { width: 100%; table-layout: fixed; border-collapse: collapse; font-size: var(--fs-meta); }
  thead th { text-align: left; color: var(--muted); font-weight: var(--weight-medium); font-size: var(--fs-sm); padding-block: var(--s0) var(--s4); }
  thead th:first-child { width: 38%; }
  td, th { padding: var(--s4) var(--s2); overflow-wrap: anywhere; vertical-align: top; text-align: left; }
  tbody th { font-weight: var(--weight-normal); }
  td { font-variant-numeric: tabular-nums; }
  small, .none { display: block; color: var(--muted); font-size: var(--fs-sm); }
  /* Число с единицей не рвётся: «0,7 с» и «1 240 ток/с» целиком. */
  .num { display: block; white-space: nowrap; }
  .empty { margin: var(--s0); padding: var(--s8); background: var(--surface); border-radius: var(--radius-card); color: var(--muted); font-size: var(--fs-meta); }
</style>
