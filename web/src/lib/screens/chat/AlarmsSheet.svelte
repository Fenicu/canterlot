<script lang="ts">
  import { fmt } from '#lib/format/ru.js';
  import type { ChatStore } from '#lib/stores/chat.svelte.js';
  import Sheet from '#lib/ui/Sheet.svelte';
  import AlarmsList from './AlarmsList.svelte';

  let { chat, onclose }: { chat: ChatStore; onclose: () => void } = $props();

  const count = $derived(
    chat.alarms.filter((alarm) => alarm.state === 'armed' || alarm.state === 'failed').length
  );
  const subtitle = $derived(
    count > 0 ? `${count} ${fmt.plural(count, ['будильник', 'будильника', 'будильников'])}` : undefined
  );
</script>

<Sheet open title="Будильники" {subtitle} {onclose}>
  <AlarmsList {chat} />
</Sheet>
