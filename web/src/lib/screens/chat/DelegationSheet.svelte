<script lang="ts">
	import { resolve } from '$app/paths';
	import type { ChatStore } from '#lib/stores/chat.svelte.js';
	import Sheet from '#lib/ui/Sheet.svelte';
	import DelegationTree from './DelegationTree.svelte';

	let { chat, onclose }: { chat: ChatStore; onclose: () => void } = $props();
</script>

<Sheet open title="Делегирование" {onclose}>
	<DelegationTree {chat} href={(blockId) => resolve(`/c/${chat.id}/lane/${blockId}` as '/')} />
</Sheet>
