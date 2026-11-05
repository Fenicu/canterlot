<script lang="ts">
	import { fmt } from '#lib/format/ru.js';
	import type { ChatStore, OwnerInputView } from '#lib/stores/chat.svelte.js';
	import Sheet from '#lib/ui/Sheet.svelte';
	import OwnerMessages from './OwnerMessages.svelte';

	let {
		chat,
		onpick,
		onclose
	}: {
		chat: ChatStore;
		onpick: (item: OwnerInputView) => void;
		onclose: () => void;
	} = $props();

	let count = $state<number | null>(null);
	const subtitle = $derived(
		count === null ? undefined : `${count} ${fmt.plural(count, ['сообщение', 'сообщения', 'сообщений'])}`
	);
</script>

<Sheet open title="Мои сообщения" {subtitle} {onclose}>
	<OwnerMessages {chat} {onpick} oncount={(value) => (count = value)} />
</Sheet>
