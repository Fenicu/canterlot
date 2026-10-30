<script lang="ts">
	import { page } from '$app/state';
	import { onMount } from 'svelte';
	import { clock } from '#lib/clock.js';
	import Lane from '#lib/screens/chat/Lane.svelte';
	import { leaseChatSession } from '#lib/screens/chat/open.js';
	import type { ChatStore } from '#lib/stores/chat.svelte.js';

	let chat = $state<ChatStore | null>(null);
	let error = $state('');
	let now = $state(clock.serverNow());
	const id = $derived(page.params.id ?? '');
	const blockId = $derived(page.params.blockId ?? '');

	onMount(() => {
		const timer = setInterval(() => (now = clock.serverNow()), 1000);
		return () => clearInterval(timer);
	});

	$effect(() => {
		const current = id;
		chat = null;
		error = '';
		if (!current) return;
		const lease = leaseChatSession(current);
		let alive = true;
		lease.chat
			.then((store) => {
				if (alive) chat = store;
			})
			.catch(() => {
				if (alive) error = 'Не удалось открыть сессию';
			});
		return () => {
			alive = false;
			lease.release();
		};
	});
</script>

<svelte:head><title>Субагент · canterlot</title></svelte:head>
<main id="main" class="lane-route">
	{#key id}
		{#if chat && chat.id === id}
			<Lane {chat} rootBlockId={blockId} {now} />
		{:else if error}
			<p>{error}</p>
		{:else}
			<p>Открываю поток субагента…</p>
		{/if}
	{/key}
</main>

<style>
	.lane-route {
		height: 100%;
		min-height: 0;
		display: flex;
		flex-direction: column;
	}
</style>
