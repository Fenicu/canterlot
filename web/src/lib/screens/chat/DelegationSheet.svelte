<script lang="ts">
	import { resolve } from '$app/paths';
	import { getRuntime, type Runtime } from '#lib/app/runtime.js';
	import type { ChatStore } from '#lib/stores/chat.svelte.js';
	import Sheet from '#lib/ui/Sheet.svelte';
	import DelegationTabs from './DelegationTabs.svelte';
	import DelegationTree from './DelegationTree.svelte';

	let { chat, onclose }: { chat: ChatStore; onclose: () => void } = $props();
	let runtime: Runtime | null = null;
	try {
		runtime = getRuntime();
	} catch {
		runtime = null;
	}
	// С планом вкладки сами называют обе части: заголовок не повторяет первую.
	const title = $derived(runtime?.list.plan.has(chat.id) ? 'Делегирование и план' : 'Делегирование');
</script>

<Sheet open {title} {onclose}>
	<DelegationTabs {chat} onnavigate={onclose}>
		<DelegationTree {chat} href={(blockId) => resolve(`/c/${chat.id}/lane/${blockId}` as '/')} />
	</DelegationTabs>
</Sheet>
