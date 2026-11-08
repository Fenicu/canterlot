<script module lang="ts">
	import { SearchStore } from './store.svelte';
	import { liveSearch } from './live';
	import { emptyState } from './model';

	// Панель стола переживает переход между сессиями и закрытие: запрос держится в памяти вкладки.
	let shared: SearchStore | null = null;
	let applied = 0;
</script>

<script lang="ts">
	import { untrack } from 'svelte';
	import IconButton from '#lib/ui/IconButton.svelte';
	import SearchView from './SearchView.svelte';
	import { searchRequest } from './open.svelte';
	import type { SearchHit } from './model';

	let { onpick, onclose }: { onpick: (hit: SearchHit) => void; onclose?: () => void } = $props();

	const store = (shared ??= new SearchStore(liveSearch, emptyState()));
	if (searchRequest.nonce !== applied) {
		applied = searchRequest.nonce;
		if (searchRequest.seed) store.restore(searchRequest.seed);
	}

	$effect(() => {
		const nonce = searchRequest.nonce;
		if (nonce === applied) return;
		untrack(() => {
			applied = nonce;
			if (searchRequest.seed) store.restore(searchRequest.seed);
			void store.run();
		});
	});
</script>

<aside class="search-panel" aria-label="Поиск по сообщениям" data-search-panel>
	<header class="topbar">
		<h2 class="panel-title">Поиск</h2>
		{#if onclose}
			<IconButton icon="close" label="Закрыть боковую панель" onclick={onclose} />
		{/if}
	</header>
	<SearchView {store} {onpick} autofocus={!store.q} />
</aside>

<style>
	.search-panel {
		display: flex;
		flex-direction: column;
		width: 100%;
		height: 100%;
		min-width: 0;
		min-height: 0;
		color: var(--text);
	}
	.topbar {
		display: flex;
		align-items: center;
		gap: var(--s4);
		min-height: var(--target);
		margin: var(--s5) var(--gutter) var(--s3);
		padding-block: var(--s2);
	}
	.topbar > :global(:last-child) {
		margin-right: var(--icon-hang);
	}
	.panel-title {
		flex: 1;
		min-width: 0;
		margin: var(--s0);
		font-size: var(--fs-title);
		font-weight: var(--weight-semibold);
	}
</style>
