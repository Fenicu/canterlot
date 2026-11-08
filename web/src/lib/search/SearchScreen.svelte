<script lang="ts">
	import { onDestroy } from 'svelte';
	import { goto, replaceState } from '$app/navigation';
	import { page } from '$app/state';
	import { resolve } from '$app/paths';
	import TopBar from '#lib/ui/TopBar.svelte';
	import Icon from '#lib/ui/Icon.svelte';
	import IconButton from '#lib/ui/IconButton.svelte';
	import { theme } from '#lib/theme.js';
	import SearchView from './SearchView.svelte';
	import { liveSearch } from './live';
	import { hitHref, readState, type SearchHit, type SearchState } from './model';
	import { SearchStore } from './store.svelte';

	// Поиск, открытый из приложения, несёт состояние в записи истории: «Назад» возвращает туда, откуда пришли.
	const opened = page.state.search !== undefined;
	const store = new SearchStore(liveSearch, readState(page.state.search));
	onDestroy(() => store.dispose());

	let saveTimer: ReturnType<typeof setTimeout> | null = null;
	onDestroy(() => {
		if (saveTimer) clearTimeout(saveTimer);
	});

	/** Запрос живёт в записи истории: «Назад» из ленты возвращает его, адрес остаётся `/search`. */
	function remember(state: SearchState): void {
		if (saveTimer) clearTimeout(saveTimer);
		saveTimer = setTimeout(() => {
			saveTimer = null;
			replaceState('', { ...page.state, search: state });
		}, 200);
	}

	function pick(hit: SearchHit): void {
		if (saveTimer) {
			clearTimeout(saveTimer);
			saveTimer = null;
		}
		replaceState('', { ...page.state, search: store.state });
		// eslint-disable-next-line svelte/no-navigation-without-resolve
		void goto(hitHref(hit));
	}
</script>

<main id="main" tabindex="-1" class="search-screen" data-search-screen>
	<TopBar title="Поиск">
		{#snippet leading()}
			{#if opened}
				<IconButton icon="back" label="Назад" onclick={() => history.back()} />
			{:else}
				<a class="back" href={resolve('/sessions')} aria-label="К сессиям"><Icon name="back" /></a>
			{/if}
		{/snippet}
		{#snippet actions()}<IconButton icon={theme.effective === 'dark' ? 'sun' : 'moon'} label="Сменить тему" onclick={() => theme.toggle()} />{/snippet}
	</TopBar>
	<SearchView {store} onpick={pick} onchange={remember} autofocus={!store.session && !store.q} />
</main>

<style>
	.back {
		display: flex;
		align-items: center;
		justify-content: center;
		min-width: var(--target);
		min-height: var(--target);
		padding: var(--s4);
		color: var(--text);
	}
	.search-screen {
		display: flex;
		flex: 1;
		flex-direction: column;
		min-width: 0;
		min-height: 0;
		max-width: var(--reading-max);
		width: 100%;
		margin-inline: auto;
	}
</style>
