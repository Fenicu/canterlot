<script lang="ts">
  import {
    blockquoteSources,
    copyBlock,
    copyButtonFrom,
    decorateCode,
    decorateQuotes
  } from './copy.js';

  interface Props {
    html: string;
    class?: string;
    /** Агентский текст: блоки кода получают кнопку копирования. */
    copy?: boolean;
    /** Исходник блока; нужен, чтобы скопировать цитату как markdown. */
    source?: string;
    final?: boolean;
  }
  const { html, class: className = '', copy = false, source, final = false }: Props = $props();
  let host: HTMLDivElement;

  // Широкий код прокручивается вбок: с клавиатуры до него можно дойти Tab и листать стрелками.
  $effect(() => {
    void html;
    for (const pre of host.querySelectorAll('pre')) {
      pre.tabIndex = 0;
      pre.setAttribute('role', 'region');
      pre.setAttribute('aria-label', 'Код');
    }
  });

  // Кнопки ставятся поверх уже очищенного HTML: разметка из ответа их не подделает.
  $effect(() => {
    void html;
    void source;
    void final;
    if (!copy) return;
    decorateCode(host);
    decorateQuotes(host, final && source ? blockquoteSources(source) : []);
  });

  async function onClick(event: MouseEvent) {
    const button = copyButtonFrom(event.target);
    if (button && host.contains(button)) await copyBlock(button);
  }

  $effect(() => {
    host.addEventListener('click', onClick);
    return () => host.removeEventListener('click', onClick);
  });
</script>

<div bind:this={host} class={className}>{@html html}</div>
