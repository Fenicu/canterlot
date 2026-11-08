<script lang="ts">
	import { resolve } from '$app/paths';
	import type { ChatStore } from '#lib/stores/chat.svelte.js';
	import Sheet from '#lib/ui/Sheet.svelte';
	import DelegationTabs from './DelegationTabs.svelte';
	import DelegationTree from './DelegationTree.svelte';

	let { chat, onclose }: { chat: ChatStore; onclose: () => void } = $props();
</script>

<Sheet open title="Делегирование" {onclose}>
	<DelegationTabs {chat} onnavigate={onclose}>
		<DelegationTree {chat} href={(blockId) => resolve(`/c/${chat.id}/lane/${blockId}` as '/')} />
	</DelegationTabs>
</Sheet>
