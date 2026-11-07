<script lang="ts">
  import type { SheetView } from './protocol.js';

  let { sheets }: { sheets: SheetView[] } = $props();
  const id = $props.id();
  let active = $state(0);
  let grid = $state<HTMLDivElement>();
  const tabs: HTMLButtonElement[] = [];

  const sheet = $derived(sheets[active] ?? sheets[0]!);
  const head = $derived(sheet.rows[0] ?? []);
  const body = $derived(sheet.rows.slice(1));
  // Кириллица шире цифры, по которой считается ch: небольшой запас, чтобы короткие подписи не переносились.
  const width = (chars: number) => `calc(${chars * 1.15}ch + 2 * var(--s5) + var(--hairline))`;
  // Обрезанный многоточием текст виден целиком в подсказке.
  const long = (text: string) => (text.length > 40 ? text : undefined);
  const tableWidth = $derived(`calc(${sheet.widths.reduce((sum, chars) => sum + chars * 1.15, 0)}ch + ${sheet.widths.length} * (2 * var(--s5) + var(--hairline)))`);

  /** «из 2201 строки», «из 2205 строк»: после «из» родительный падеж. */
  const of = (count: number, one: string, many: string) => `${count} ${count % 10 === 1 && count % 100 !== 11 ? one : many}`;
  const note = $derived.by(() => {
    const parts: string[] = [];
    if (sheet.rows.length < sheet.totalRows) parts.push(`${sheet.rows.length} из ${of(sheet.totalRows, 'строки', 'строк')}`);
    if (sheet.shownColumns < sheet.totalColumns) parts.push(`${sheet.shownColumns} из ${of(sheet.totalColumns, 'столбца', 'столбцов')}`);
    return parts.length ? `Показаны первые ${parts.join(' и ')}. Остальное — в скачанном файле.` : '';
  });

  /**
   * В DOM только видимые строки и запас вокруг: две тысячи строк разом — это секунды раскладки
   * на телефоне. Строки одной высоты (без переносов), поэтому окно считается делением.
   */
  const OVERSCAN = 20;
  const ESTIMATED_ROW = 30;
  let rowHeight = $state(0);
  let viewport = $state({ top: 0, height: 0 });
  const step = $derived(rowHeight || ESTIMATED_ROW);
  const start = $derived(Math.max(0, Math.floor(viewport.top / step) - OVERSCAN));
  const end = $derived(Math.min(body.length, Math.ceil((viewport.top + (viewport.height || ESTIMATED_ROW * 40)) / step) + OVERSCAN));
  const visible = $derived(body.slice(start, end));

  let frame = 0;
  function measure() {
    if (grid) viewport = { top: grid.scrollTop, height: grid.clientHeight };
  }
  function onscroll() {
    if (frame) return;
    frame = requestAnimationFrame(() => {
      frame = 0;
      measure();
    });
  }

  $effect(() => {
    if (!grid) return;
    // Поворот экрана или смена размера шрифта меняют и окно, и высоту строки.
    const observer = new ResizeObserver(() => {
      rowHeight = 0;
      measure();
    });
    observer.observe(grid);
    return () => {
      observer.disconnect();
      cancelAnimationFrame(frame);
    };
  });

  // Высота строки — по первой отрисованной: шрифт, поля и рамки у всех строк одни.
  $effect(() => {
    void visible;
    if (rowHeight || !grid) return;
    const row = grid.querySelector('tbody tr[aria-rowindex]');
    if (row) rowHeight = row.getBoundingClientRect().height;
  });

  function pick(index: number, focus = false) {
    // Новый лист открывается с начала. Прокрутку сбрасываем до смены листа: прежняя таблица уже разложена.
    if (index !== active && grid) {
      grid.scrollTop = 0;
      grid.scrollLeft = 0;
      viewport = { top: 0, height: grid.clientHeight };
    }
    active = index;
    if (focus) tabs[index]?.focus();
  }

  function keydown(event: KeyboardEvent, index: number) {
    const last = sheets.length - 1;
    const next = { ArrowRight: index + 1, ArrowLeft: index - 1, Home: 0, End: last }[event.key];
    if (next === undefined) return;
    event.preventDefault();
    pick(next < 0 ? last : next > last ? 0 : next, true);
  }
</script>

<div class="sheet-grid">
  {#if sheets.length > 1}
    <div class="tabs" role="tablist" aria-label="Листы">
      {#each sheets as item, index (index)}
        <button
          type="button"
          role="tab"
          id={`${id}-tab-${index}`}
          aria-selected={index === active}
          aria-controls={`${id}-panel`}
          tabindex={index === active ? 0 : -1}
          bind:this={tabs[index]}
          onclick={() => pick(index)}
          onkeydown={(event) => keydown(event, index)}
        >{item.name || `Лист ${index + 1}`}</button>
      {/each}
    </div>
  {/if}
  {#if note}<p class="note" data-role="sheet-cap">{note}</p>{/if}
  <!-- Сетка прокручивается в обе стороны: с клавиатуры до неё доходят Tab и листают стрелками. -->
  <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
  <div
    class="grid"
    id={`${id}-panel`}
    role={sheets.length > 1 ? 'tabpanel' : 'region'}
    aria-labelledby={sheets.length > 1 ? `${id}-tab-${active}` : undefined}
    aria-label={sheets.length > 1 ? undefined : `Лист ${sheet.name}`}
    tabindex="0"
    bind:this={grid}
    {onscroll}
  >
    {#if sheet.rows.length === 0}
      <p class="empty">Лист пуст</p>
    {:else}
      <table style:width={tableWidth} aria-rowcount={sheet.rows.length}>
        <colgroup>{#each sheet.widths as chars, column (column)}<col style:width={width(chars)} />{/each}</colgroup>
        <!-- Подпись числового столбца стоит над числами, у правого края. -->
        <thead>
          <tr aria-rowindex="1">{#each head as cell, column (column)}<th scope="col" data-numeric={cell.numeric || body[0]?.[column]?.numeric || undefined} title={long(cell.text)}>{cell.text}</th>{/each}</tr>
        </thead>
        <tbody>
          {#if start > 0}<tr class="spacer" aria-hidden="true"><td colspan={head.length} style:height={`${start * step}px`}></td></tr>{/if}
          {#each visible as row, index (start + index)}
            <tr aria-rowindex={start + index + 2}>
              {#each row as cell, column (column)}
                {#if column === 0}
                  <th scope="row" data-numeric={cell.numeric || undefined} title={long(cell.text)}>{cell.text}</th>
                {:else}
                  <td data-numeric={cell.numeric || undefined} title={long(cell.text)}>{cell.text}</td>
                {/if}
              {/each}
            </tr>
          {/each}
          {#if end < body.length}<tr class="spacer" aria-hidden="true"><td colspan={head.length} style:height={`${(body.length - end) * step}px`}></td></tr>{/if}
        </tbody>
      </table>
    {/if}
  </div>
</div>

<style>
  .sheet-grid { display: flex; flex-direction: column; gap: var(--s4); flex: 1 1 auto; min-width: 0; min-height: 0; }
  .tabs { display: flex; flex: none; gap: var(--s2); max-width: 100%; padding: var(--s2); overflow-x: auto; border-radius: var(--radius-card); background: var(--raised); scrollbar-width: none; }
  .tabs button { flex: none; min-height: var(--target); padding-inline: var(--s6); border-radius: var(--radius-pill); color: var(--text); font-size: var(--fs-meta); white-space: nowrap; }
  .tabs button[aria-selected='true'] { background: var(--accent); color: var(--accent-ink); font-weight: var(--weight-medium); }
  .tabs button:focus-visible { outline: var(--focus-width) solid var(--accent); outline-offset: calc(-1 * var(--focus-width)); }
  .note { flex: none; margin: var(--s0); color: var(--secondary); font-size: var(--fs-sm); }
  /* Таблица прокручивается в своей рамке в обе стороны; страница и лист остаются на месте. */
  /* Сетка — единственный горизонтальный скроллер листа. `pan-x pan-y` отдаёт ей касание
     в обе стороны, а инерция нужна и вбок: без неё широкий лист кажется застрявшим. */
  .grid { flex: 1 1 auto; min-width: 0; min-height: 0; max-width: 100%; overflow: auto; overscroll-behavior: contain; touch-action: pan-x pan-y; -webkit-overflow-scrolling: touch; border: var(--hairline) solid var(--line); border-radius: var(--radius-sm); background: var(--surface); }
  .grid:focus-visible { outline: var(--focus-width) solid var(--accent); }
  .empty { margin: var(--s0); padding: var(--s8); color: var(--secondary); }
  /* Раскладка по заданным ширинам: браузер не меряет каждую из тысяч ячеек. */
  table { table-layout: fixed; border-collapse: separate; border-spacing: var(--s0); font-size: var(--fs-meta); line-height: var(--lh-tight); font-variant-numeric: tabular-nums; }
  /* Всё в одну строку: у строк одна высота, и окно прокрутки считается без замеров каждой. */
  th, td { padding: var(--s3) var(--s5); overflow: hidden; border-right: var(--hairline) solid var(--line); border-bottom: var(--hairline) solid var(--line); text-align: left; vertical-align: top; white-space: nowrap; text-overflow: ellipsis; }
  [data-numeric] { text-align: right; }
  .spacer td { padding: var(--s0); border: var(--s0); }
  thead th { position: sticky; top: var(--s0); z-index: 2; background: var(--raised); color: var(--text); font-weight: var(--weight-medium); }
  tbody th { position: sticky; left: var(--s0); z-index: 1; background: var(--surface); color: var(--text); font-weight: var(--weight-normal); }
  thead th:first-child { left: var(--s0); z-index: 3; }
</style>
