<script lang="ts">
  interface Props {
    html: string;
    class?: string;
  }
  const { html, class: className = '' }: Props = $props();
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
</script>

<div bind:this={host} class={className}>{@html html}</div>
