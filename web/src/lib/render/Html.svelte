<script lang="ts">
  interface Props {
    html: string;
    class?: string;
  }
  const { html, class: className = '' }: Props = $props();
</script>

<div class={className}>{@html html}</div>
