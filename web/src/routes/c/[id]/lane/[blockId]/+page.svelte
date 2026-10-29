<script lang="ts">
	import { page } from '$app/state';
	import { onMount } from 'svelte';
	import Lane from '#lib/screens/chat/Lane.svelte';
	import { openChatSession } from '#lib/screens/chat/open.js';
	import type { ChatStore } from '#lib/stores/chat.svelte.js';

	let chat = $state<ChatStore | null>(null);
	let error = $state('');
	const id = $derived(page.params.id ?? '');
	const blockId = $derived(page.params.blockId ?? '');

	onMount(() => {
		let alive = true;
		openChatSession(id)
			.then((store) => {
				if (alive) chat = store;
			})
			.catch(() => {
				error = 'Не удалось открыть сессию';
			});
		return () => {
			alive = false;
		};
	});
</script>

<svelte:head><title>Субагент · canterlot</title></svelte:head>
<main id="main" class="lane-route">
	{#if chat}
		<Lane {chat} rootBlockId={blockId} />
	{:else if error}
		<p>{error}</p>
	{:else}
		<p>Открываю поток субагента…</p>
	{/if}
</main>

<style>
	.lane-route {
		height: 100%;
		min-height: 0;
		display: flex;
		flex-direction: column;
	}
</style>
