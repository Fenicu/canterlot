<script lang="ts">
  import type { ChatStore } from '$lib/stores/chat.svelte';
  import PendingStrip from '$lib/timeline/PendingStrip.svelte';
  import Timeline from '$lib/timeline/Timeline.svelte';

  let { chat, onInfo, now = Date.now() }: { chat: ChatStore; onInfo?: (runId: string) => void; now?: number } = $props();
</script>

<section class="screen" data-ready>
  <Timeline {chat} {onInfo} {now} />
  <PendingStrip {chat} />
</section>

<style>
  .screen { height: 100dvh; display: flex; flex-direction: column; min-width: 0; }
</style>
