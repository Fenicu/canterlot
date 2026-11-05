<script lang="ts">
  import { PALETTES, theme } from '#lib/theme.js';
</script>
<!-- Нативные radio дают стрелки, выбор и состояние для экранного диктора; карточка — их подпись. -->
<div class="grid" role="radiogroup" aria-label="Тема">
  {#each PALETTES as palette (palette.key)}
    <label class="option" data-option={palette.key}>
      <input type="radio" name="palette" value={palette.key} checked={theme.palette === palette.key} onchange={() => theme.setPalette(palette.key)} />
      <!-- Предпросмотр рисуют токены самой палитры: атрибуты на узле переопределяют цвета только внутри него. -->
      <span class="preview" data-palette={palette.key} data-theme={theme.effective} aria-hidden="true">
        <span class="bar"><span class="dot"></span><span class="title"></span><span class="action"></span></span>
        <span class="feed">
          <span class="line"></span>
          <span class="line short"></span>
          <span class="code"><span class="kw">const</span> <span class="var">ok</span> = <span class="str">"да"</span></span>
          <span class="owner"><span class="line"></span></span>
        </span>
      </span>
      <span class="caption"><span class="mark"></span><span class="name">{palette.name}</span></span>
    </label>
  {/each}
</div>
<style>
  .grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(9.5rem, 1fr)); gap: var(--s4); }
  /* Рамок в языке интерфейса нет: выбранную карточку обводит кольцо акцента внутри неё, рядом заполненная метка и жирная подпись. */
  .option { position: relative; display: flex; flex-direction: column; gap: var(--s3); min-width: 0; min-height: var(--target); padding: var(--s3); border-radius: var(--radius-card); background: var(--card); cursor: pointer; --card: var(--raised); }
  .option:has(input:checked) { outline: var(--focus-width) solid var(--accent); outline-offset: calc(var(--focus-width) * -1); }
  /* Фокус в группе radio всегда на выбранной карточке: кольцо выходит наружу, как у фокуса во всём приложении. */
  .option:has(input:focus-visible) { outline: var(--focus-width) solid var(--accent); outline-offset: var(--s1); }
  @media (hover: hover) and (pointer: fine) { .option:hover { --card: var(--hover); } }
  /* Невидимый radio накрывает всю карточку: нажатие в любом месте попадает в него самого. */
  input { position: absolute; inset: var(--s0); z-index: 1; width: 100%; height: 100%; margin: var(--s0); opacity: 0; cursor: pointer; }
  .preview { display: flex; flex-direction: column; overflow: hidden; border-radius: var(--radius-sm); background: var(--chat-bg); color: var(--text); font-size: var(--fs-xs); line-height: var(--lh-tight); }
  .bar { display: flex; align-items: center; gap: var(--s2); padding: var(--s3) var(--s4); background: var(--surface); }
  .dot { flex: none; width: var(--mark); height: var(--mark); border-radius: var(--radius-round); background: var(--success); }
  .title { flex: 1; height: var(--s2); max-width: 40%; border-radius: var(--radius-pill); background: var(--text); }
  .action { flex: none; width: var(--s10); height: var(--s5); margin-left: auto; border-radius: var(--radius-pill); background: var(--accent); }
  .feed { display: flex; flex-direction: column; gap: var(--s2); padding: var(--s3) var(--s4) var(--s4); }
  .line { display: block; height: var(--s2); width: 86%; border-radius: var(--radius-pill); background: var(--secondary); }
  .line.short { width: 58%; }
  .code { display: block; padding: var(--s1) var(--s3); overflow: hidden; border-radius: var(--radius-pill); background: var(--code); color: var(--syn-variable); font-family: var(--font-mono); white-space: nowrap; }
  .kw { color: var(--syn-keyword); }
  .var { color: var(--syn-variable); }
  .str { color: var(--syn-string); }
  .owner { align-self: flex-end; width: 56%; padding: var(--s2) var(--s3); border-radius: var(--radius-pill); background: var(--accent-soft); }
  .owner .line { width: 100%; background: var(--text); }
  .caption { display: flex; align-items: center; gap: var(--s3); min-height: var(--s10); padding-inline: var(--s3); font-size: var(--fs-meta); }
  /* Метка как у radio: пустое кольцо и заполненный круг с точкой. Кольцо — внутренний круг поверх заливки, без рамки. */
  .mark { position: relative; flex: none; width: var(--icon-small); height: var(--icon-small); border-radius: var(--radius-round); background: var(--muted); }
  .mark::after { content: ''; position: absolute; inset: var(--focus-width); border-radius: var(--radius-round); background: var(--card); }
  .option:has(input:checked) .mark { background: var(--accent); }
  .option:has(input:checked) .mark::after { inset: var(--s2); background: var(--accent-ink); }
  .option:has(input:checked) .name { font-weight: var(--weight-semibold); }
  .name { min-width: 0; overflow-wrap: anywhere; }
</style>
