<script lang="ts">
  import type { Snippet } from "svelte";
  let { title, children }: { title: string; children: Snippet } = $props();
</script>

<section aria-label={title}>{@render children()}</section>
