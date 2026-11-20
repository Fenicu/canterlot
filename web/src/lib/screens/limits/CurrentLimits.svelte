<script lang="ts">
  import Gauge from '#lib/ui/Gauge.svelte';
  import { timeTip } from '#lib/ui/time-tip.js';
  import type { currentBlocks } from './model.js';
  let { blocks }: { blocks: ReturnType<typeof currentBlocks> } = $props();
</script>
<div class="current">
  {#each blocks as block (block.key)}
    <section class="provider" data-provider={block.provider} data-account={block.named ? block.account : undefined} data-tone={block.lead?.tone} data-stale={block.stale || undefined} aria-label="{block.label}, текущие лимиты">
      <header class="head">
        <h3>{block.label}</h3>
        <!-- Крупно — самое тесное свежее окно: сколько осталось там, где кончится раньше всего. -->
        {#if block.lead}<p class="lead" data-window={block.lead.key}><b>{block.lead.pct}</b><span>{block.lead.caption}</span></p>
        {:else if block.stale}<p class="lead"><b>устарело</b><span>{block.seen}</span></p>{/if}
        {#if block.status || block.markers.length || block.providerStatus}<p class="status">{#if block.status}<span class="word">{block.status}</span>{/if}{#each block.markers as marker (marker)}<span class="marker" class:reserve={marker === 'резерв недели'}>{marker}</span>{/each}{#if block.providerStatus}
          <!-- eslint-disable-next-line svelte/no-navigation-without-resolve -- внешняя страница статуса провайдера -->
          <a class="pstatus" data-status-tone={block.providerStatus.tone} href={block.providerStatus.url} target="_blank" rel="noopener noreferrer" title={block.providerStatus.title} aria-label={block.providerStatus.title}>{block.providerStatus.company}: {block.providerStatus.text}</a>{/if}</p>{/if}
      </header>
      {#each block.credentials as line, index (index)}<p class="credential" data-provider={block.provider} data-tone={line.tone}>{line.text}</p>{/each}
      {#if block.credits.length}
        <ul class="credits" aria-label="Сбросы лимитов">
          {#each block.credits as credit (credit.key)}
            <li data-provider={block.provider}>{credit.title}{#if credit.expires}<span class="expires">&nbsp;— {credit.expires}</span>{/if}</li>
          {/each}
        </ul>
      {/if}
      {#if block.empty}<p class="none">нет данных</p>
      {:else}<div class="rows">
        {#each block.windows as row (row.key)}
          <div class="row" data-window={row.key} data-tone={row.tone} data-stale={row.stale || undefined}>
            <span class="label">{row.label}</span>
            <b class="value">{row.value}</b>
            {#if row.reset}<time class="reset" data-short={row.resetShort} datetime={new Date(row.resetAt).toISOString()} use:timeTip={row.resetAt}><span class="reset-long">{row.reset}</span></time>{/if}
            <span class="gauge"><Gauge value={row.fill} tone={row.tone} quiet dim={row.stale} label="{block.label}, {row.label}" /></span>
            <!-- Прогноз по средней скорости окна: предупреждение, только если окно кончится раньше сброса. -->
            {#if row.forecast}<p class="forecast" data-forecast={row.forecast.before ? 'before' : 'calm'}><span class="verdict">{row.forecast.text}</span><span class="pace" title="Средняя скорость расхода окна">{row.forecast.pace}</span></p>{/if}
          </div>
        {/each}
      </div>{/if}
    </section>
  {/each}
</div>
<style>
  /* Карточки делят ширину поровну: на телефоне столбиком, на рабочем столе в ряд. Та же сетка у недельных графиков ниже. */
  .current { display: grid; grid-template-columns: repeat(auto-fit, minmax(min(100%, var(--limits-column)), 1fr)); gap: var(--s4); }
  .provider { min-width: 0; container-type: inline-size; padding: var(--s6) var(--s8) var(--s8); background: var(--surface); border-radius: var(--radius-card); }
  .head { display: grid; grid-template-columns: minmax(0, 1fr) max-content; grid-template-areas: 'name lead' 'status lead'; align-items: start; column-gap: var(--s4); margin-bottom: var(--s6); }
  h3 { grid-area: name; min-width: 0; overflow-wrap: anywhere; margin: var(--s0); font-size: var(--fs-body); font-weight: var(--weight-semibold); line-height: var(--lh-tight); padding-top: var(--s2); }
  .lead { grid-area: lead; display: flex; flex-direction: column; align-items: end; margin: var(--s0); text-align: end; }
  .lead b { font-size: var(--fs-heading); font-weight: var(--weight-semibold); line-height: var(--lh-tight); font-variant-numeric: tabular-nums; letter-spacing: var(--tracking-tight); }
  .lead span { color: var(--muted); font-size: var(--fs-xs); white-space: nowrap; }
  [data-tone='warning'] .lead b, [data-tone='warning'] .word { color: var(--warning); }
  [data-tone='danger'] .lead b, [data-tone='danger'] .word { color: var(--danger); }
  [data-stale] .lead b { color: var(--muted); font-size: var(--fs-title); font-weight: var(--weight-normal); }
  .status { grid-area: status; display: flex; flex-wrap: wrap; gap: var(--s1) var(--s4); margin: var(--s2) var(--s0) var(--s0); font-size: var(--fs-sm); color: var(--muted); }
  .word { font-weight: var(--weight-semibold); }
  .marker.reserve { color: var(--warning); }
  .pstatus { font-weight: var(--weight-semibold); text-decoration: none; }
  .pstatus[data-status-tone='warning'] { color: var(--warning); }
  .pstatus[data-status-tone='danger'] { color: var(--danger); }
  .pstatus:hover, .pstatus:focus-visible { text-decoration: underline; }
  /* Подпись, остаток и сброс — одна строка над полосой; колонки общие, значения окон стоят друг под другом. */
  .rows { display: grid; grid-template-columns: max-content minmax(0, 1fr) max-content; column-gap: var(--s4); row-gap: var(--s6); }
  .row { display: grid; grid-template-columns: subgrid; grid-column: 1 / -1; align-items: center; row-gap: var(--s3); }
  .label { grid-column: 1; }
  .value { grid-column: 2; }
  .reset { grid-column: 3; justify-self: end; }
  .gauge { grid-column: 1 / -1; grid-row: 2; display: flex; min-width: 0; --gauge-width: 100%; }
  .forecast { grid-column: 1 / -1; grid-row: 3; display: flex; flex-wrap: wrap; justify-content: space-between; gap: var(--s1) var(--s4); margin: var(--s0); font-size: var(--fs-sm); color: var(--muted); }
  .forecast[data-forecast='before'] .verdict { color: var(--warning); font-weight: var(--weight-semibold); }
  .pace { font-variant-numeric: tabular-nums; white-space: nowrap; }
  .label, .reset { color: var(--muted); font-size: var(--fs-sm); white-space: nowrap; }
  .value { font-size: var(--fs-meta); font-weight: var(--weight-semibold); font-variant-numeric: tabular-nums; white-space: nowrap; }
  .reset { font-variant-numeric: tabular-nums; }
  .row[data-tone='warning'] .value { color: var(--warning); }
  .row[data-tone='danger'] .value { color: var(--danger); }
  .row[data-stale] .value { color: var(--muted); font-weight: var(--weight-normal); }
  .none { margin: var(--s0); color: var(--muted); font-size: var(--fs-meta); }
  /* Конец входа провайдера: дата под шапкой карточки, тревогу несут только слова. */
  .credential { margin: var(--s0) 0 var(--s6); color: var(--muted); font-size: var(--fs-sm); overflow-wrap: anywhere; }
  .credential[data-tone='warning'] { color: var(--warning); }
  .credential[data-tone='danger'] { color: var(--danger); }
  /* Сбросы лимитов: список под шапкой, срок — тише заголовка. */
  .credits { margin: var(--s0) 0 var(--s6); padding: var(--s0); list-style: none; color: var(--text); font-size: var(--fs-sm); }
  .credits li { overflow-wrap: anywhere; }
  .expires { color: var(--muted); }
  /* В очень узкой карточке сброс уходит под полосу: иначе он вылезает за край. */
  @container (max-width: 15em) {
    .rows { grid-template-columns: max-content minmax(0, 1fr); }
    .reset { grid-column: 1 / -1; grid-row: 3; }
    .forecast { grid-row: 4; }
  }
  /* В тесной карточке (296 px при 360) полному «сброс через 4 ч 58 мин» не хватает места рядом
     со значением: видимый сжатый отсчёт рисует псевдоэлемент, а полный текст остаётся в разметке
     невидимым для глаза, но доступным экранному читалке. У <time> нет роли, поэтому aria-label
     там запрещён. */
  @container (max-width: 20.5em) {
    .reset-long { position: absolute; width: var(--hairline); height: var(--hairline); padding: 0; margin: 0; overflow: hidden; clip-path: inset(50%); white-space: nowrap; }
    /* Пустой альтернативный текст: сжатый отсчёт декоративен и не читается вторым разом. */
    .reset::after { content: attr(data-short) / ''; }
  }
</style>
