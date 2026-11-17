<script lang="ts" module>
  /** Строка кода; у разницы файла — добавленная, удалённая, контекст или заголовок куска. */
  export interface CodeLine {
    text: string;
    kind?: 'add' | 'del' | 'ctx' | 'hunk';
  }
</script>

<script lang="ts">
  import { untrack } from 'svelte';
  import { highlightLines, type CodeToken } from '#lib/render/highlight.js';

  let {
    lines,
    lang,
    label,
    signs = false
  }: {
    lines: CodeLine[];
    lang: string | null;
    /** Имя области для чтения с экрана: широкий код листается вбок с клавиатуры. */
    label: string;
    /** Знак строки разницы слева: `+`, `−`. */
    signs?: boolean;
  } = $props();

  /** Перевод строки в конце строки: выделенный и скопированный текст остаётся построчным. */
  const NEWLINE = '\n';
  const SIGN: Record<string, string> = { add: '+', del: '−', ctx: ' ', hunk: ' ' };

  /** Подсветка по строкам; заголовки кусков идут мимо неё, это не код. */
  let painted = $state<(CodeToken[] | null)[] | null>(null);
  // Карточки пересобираются каждый тик часов, но массив строк при этом тот же по содержимому.
  // Эффект зависит от подписи кода, а не от личности массива: подсветка не дрожит и не обрывается.
  const signature = $derived(`${lang ?? ''}\n${lines.map((line) => `${line.kind ?? ''}:${line.text}`).join('\n')}`);

  $effect(() => {
    const language = signature.slice(0, signature.indexOf('\n'));
    const source = untrack(() => lines);
    painted = null;
    if (!language) return;
    const code = source.filter((line) => line.kind !== 'hunk');
    let alive = true;
    void highlightLines(code.map((line) => line.text).join('\n'), language).then((rows) => {
      if (!alive || !rows) return;
      let at = 0;
      painted = source.map((line) => (line.kind === 'hunk' ? null : (rows[at++] ?? null)));
    });
    return () => {
      alive = false;
    };
  });
</script>

<!-- Код не переносится: длинная строка листается вбок внутри блока, а не всей страницей; с клавиатуры до неё доходит Tab. -->
<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
<pre class="code" tabindex="0" role="region" aria-label={label} data-signs={signs ? '' : undefined}><code>{#each lines as line, index (index)}<span class="row" data-kind={line.kind}>{#if signs}<span class="sign" aria-hidden="true">{SIGN[line.kind ?? 'ctx']}</span>{/if}{#if painted?.[index]}{#each painted[index] as token, at (at)}<span class={`token ${token.cls}`}>{token.text}</span>{/each}{:else}{line.text}{/if}{NEWLINE}</span>{/each}</code></pre>

<style>
  .code {
    min-width: 0;
    max-width: 100%;
    margin: var(--s0);
    padding-block: var(--s4);
    overflow-x: auto;
    border-radius: var(--radius-sm);
    background: var(--code);
    color: var(--text);
    font-family: var(--font-mono);
    font-size: var(--fs-sm);
    line-height: var(--lh-code);
    white-space: pre;
    overflow-wrap: normal;
    -webkit-overflow-scrolling: touch;
  }
  /* Строки растягиваются до самой длинной: фон добавленной строки идёт под весь текст при прокрутке. */
  code { display: block; width: max-content; min-width: 100%; font: inherit; }
  .row { display: block; padding-inline: var(--s6); }
  .row[data-kind='add'] { background: var(--success-soft); }
  .row[data-kind='del'] { background: var(--danger-soft); }
  .row[data-kind='hunk'] { color: var(--secondary); }
  /*
   * Цвета подсветки рассчитаны на фон кода: на мягком зелёном и красном комментарии и строки
   * части палитр теряют контраст AA. Изменённая строка остаётся строкой текста на своём фоне,
   * подсветка — у контекста вокруг и у нового файла целиком.
   */
  .row:is([data-kind='add'], [data-kind='del']) .token { color: var(--text); font-style: normal; }
  .sign { display: inline-block; width: var(--s6); color: var(--secondary); user-select: none; }
  .row[data-kind='add'] .sign { color: var(--success); }
  .row[data-kind='del'] .sign { color: var(--danger); }
</style>
