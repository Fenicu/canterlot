<script lang="ts">
	import { onMount, untrack } from 'svelte';
	import { fmt } from '#lib/format/ru.js';
	import { modelLabel } from '#lib/format/labels.js';
	import Segmented from '#lib/ui/Segmented.svelte';
	import Icon from '#lib/ui/Icon.svelte';
	import { timeTip } from '#lib/ui/time-tip.js';
	import HitSnippet from './HitSnippet.svelte';
	import { KINDS, MAX_QUERY, PERIODS, hitHarness, originLabel, totalLabel, type KindFilter, type Period, type SearchHit, type SearchState } from './model';
	import { hitKey, type SearchStore } from './store.svelte';

	let {
		store,
		onpick,
		onchange,
		autofocus = false
	}: {
		store: SearchStore;
		onpick: (hit: SearchHit) => void;
		/** Запрос или фильтр изменился: хозяин сохраняет состояние в записи истории. */
		onchange?: (state: SearchState) => void;
		autofocus?: boolean;
	} = $props();

	const DEBOUNCE_MS = 300;
	const now = Date.now();
	let field = $state<HTMLInputElement | null>(null);
	let scroller = $state<HTMLElement | null>(null);
	let sentinel = $state<HTMLElement | null>(null);
	let period = $state<string>(untrack(() => store.period));
	let kind = $state<string>(untrack(() => store.kind));
	let timer: ReturnType<typeof setTimeout> | null = null;
	let started = false;

	onMount(() => {
		if (autofocus) field?.focus();
		// Возврат к поиску из ленты повторяет запрос: старые страницы в памяти не держатся.
		void store.run();
		started = true;
		return () => {
			if (timer) clearTimeout(timer);
		};
	});

	function changed(debounce: boolean): void {
		onchange?.(store.state);
		if (timer) clearTimeout(timer);
		timer = null;
		if (!debounce) {
			void store.run();
			return;
		}
		timer = setTimeout(() => {
			timer = null;
			void store.run();
		}, DEBOUNCE_MS);
	}

	$effect(() => {
		const next = period as Period;
		if (!started || next === untrack(() => store.period)) return;
		untrack(() => {
			store.period = next;
			changed(false);
		});
	});

	$effect(() => {
		const next = kind as KindFilter;
		if (!started || next === untrack(() => store.kind)) return;
		untrack(() => {
			store.kind = next;
			changed(false);
		});
	});

	// Фильтры снаружи (новая сессия в панели стола) переставляют переключатели.
	$effect(() => {
		period = store.period;
		kind = store.kind;
	});

	function input(): void {
		changed(true);
	}

	function keydown(event: KeyboardEvent): void {
		if (event.key === 'Enter') {
			event.preventDefault();
			changed(false);
		} else if (event.key === 'Escape' && store.q) {
			event.preventDefault();
			clear();
		}
	}

	function clear(): void {
		store.q = '';
		changed(false);
		field?.focus();
	}

	function allSessions(): void {
		store.session = null;
		changed(false);
	}

	// Подгрузка следующей страницы, когда низ списка показался в области прокрутки.
	$effect(() => {
		const root = scroller;
		const node = sentinel;
		if (!root || !node) return;
		const observer = new IntersectionObserver(
			(entries) => {
				if (entries.some((entry) => entry.isIntersecting)) void store.more();
			},
			{ root, rootMargin: '0px 0px 240px 0px' }
		);
		observer.observe(node);
		return () => observer.disconnect();
	});

	const scopeText = $derived(store.session ? 'в этой сессии' : 'во всех сессиях');
</script>

<div class="search-view" data-search-view data-status={store.status}>
	<div class="controls">
		<div class="field" role="search">
			<Icon name="search" />
			<input
				bind:this={field}
				bind:value={store.q}
				type="search"
				enterkeyhint="search"
				maxlength={MAX_QUERY}
				autocomplete="off"
				aria-label="Поиск по сообщениям"
				placeholder="Слово или фраза"
				oninput={input}
				onkeydown={keydown}
			/>
			{#if store.q}<button type="button" class="reset" onclick={clear}>Сбросить</button>{/if}
		</div>
		{#if store.session}
			<div class="scope" data-search-scope>
				<span class="scope-text">В сессии «{store.session.title || 'Сессия'}»</span>
				<button type="button" class="scope-clear" aria-label="Искать во всех сессиях" onclick={allSessions}><Icon name="close" /></button>
			</div>
		{/if}
		<div class="filters">
			<Segmented name="search-period" label="Период" options={PERIODS} bind:value={period} />
			<Segmented name="search-kind" label="Что искать" options={KINDS} bind:value={kind} />
		</div>
	</div>

	<div class="results" bind:this={scroller} data-search-results>
		{#if store.status === 'idle'}
			<div class="state" data-search-empty>
				<p class="state-title">Поиск по ответам и сообщениям {scopeText}</p>
				<p class="hint">Ищутся слова целиком в любой форме. Фраза в кавычках ищется точно, слово с минусом исключается</p>
			</div>
		{:else if store.status === 'loading' && store.hits.length === 0}
			<p class="state-title state" role="status" data-search-loading>Ищу…</p>
		{:else if store.status === 'error' && store.failure}
			<div class="state" role="alert" data-search-error={store.failure.kind}>
				<p class="state-title">{store.failure.message}</p>
				<button type="button" class="retry" onclick={() => void store.run()}>Повторить</button>
			</div>
		{:else if store.status === 'ready' && store.hits.length === 0}
			<div class="state" data-search-nothing>
				<p class="state-title">Ничего не найдено</p>
				<p class="hint">Части слов, пути к файлам и фрагменты кода не находятся: ищите целые слова</p>
			</div>
		{:else}
			<p class="total" role="status" data-search-total>{totalLabel(store.total, store.capped)}</p>
			<ul class="hits">
				{#each store.hits as hit (hitKey(hit))}
					{@const at = Date.parse(hit.created_at)}
					<li>
						<button type="button" class="hit" data-search-hit={hitKey(hit)} onclick={() => onpick(hit)}>
							<span class="head">
								<span class="title">{hit.conversation_title || 'Сессия'}</span>
								<time class="time" datetime={hit.created_at} use:timeTip={at}>{fmt.messageTime(at, now)}</time>
							</span>
							<span class="meta">
								<span class="origin" data-search-origin>{originLabel(hit)}</span>
								{#if hit.archived}<span class="archived" data-search-archived>в архиве</span>{/if}
								<span class="who">{hitHarness(hit.harness)}{hit.model ? ` · ${modelLabel(hit.model)}` : ''}</span>
							</span>
							<HitSnippet segments={hit.snippet} />
						</button>
					</li>
				{/each}
			</ul>
			{#if store.moreFailure}
				<div class="more-state" role="alert">
					<p class="hint">{store.moreFailure.message}</p>
					<button type="button" class="retry" onclick={() => void store.more()}>Повторить</button>
				</div>
			{:else if store.loadingMore}
				<p class="hint more-state" role="status">Загружаю ещё…</p>
			{/if}
			<div class="sentinel" bind:this={sentinel} aria-hidden="true"></div>
		{/if}
	</div>
</div>

<style>
	.search-view {
		display: flex;
		flex: 1;
		flex-direction: column;
		min-width: 0;
		min-height: 0;
	}
	.controls {
		display: flex;
		flex-direction: column;
		gap: var(--s4);
		padding: var(--s2) var(--gutter) var(--s4);
	}
	.field {
		display: flex;
		align-items: center;
		gap: var(--s2);
		padding-inline-start: var(--s6);
		border-radius: var(--radius-pill);
		background: var(--raised);
		color: var(--secondary);
	}
	.field:has(input:focus-visible) {
		outline: var(--focus-width) solid var(--accent);
		outline-offset: var(--s1);
	}
	.field :global(svg) {
		flex: none;
		width: var(--icon-small);
		height: var(--icon-small);
	}
	input {
		flex: 1;
		min-width: 0;
		min-height: var(--target);
		padding: var(--s4) var(--s4);
		border: var(--s0);
		border-radius: var(--radius-pill);
		background: var(--transparent);
		color: var(--text);
		font-size: var(--fs-input);
	}
	input:focus-visible {
		outline: none;
	}
	input::-webkit-search-cancel-button {
		display: none;
	}
	.reset {
		flex: none;
		min-height: var(--target);
		padding-inline: var(--s6);
		border: var(--s0);
		border-radius: var(--radius-pill);
		background: var(--transparent);
		color: var(--accent-link);
		font-size: var(--fs-meta);
	}
	.scope {
		display: flex;
		align-items: center;
		align-self: flex-start;
		gap: var(--s2);
		max-width: 100%;
		padding-inline-start: var(--s6);
		border-radius: var(--radius-pill);
		background: var(--selected);
		color: var(--text);
		font-size: var(--fs-meta);
	}
	.scope-text {
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.scope-clear {
		display: inline-flex;
		flex: none;
		align-items: center;
		justify-content: center;
		min-width: var(--target);
		min-height: var(--target);
		border: var(--s0);
		border-radius: var(--radius-round);
		background: var(--transparent);
		color: var(--secondary);
	}
	.scope-clear :global(svg) {
		width: var(--icon-small);
		height: var(--icon-small);
	}
	/* Каждый фильтр — своя строка: варианты стоят в ряд и не переносятся под соседний фильтр. */
	.filters {
		display: flex;
		flex-direction: column;
		gap: var(--s3);
	}
	.results {
		flex: 1;
		min-height: 0;
		overflow-y: auto;
		overscroll-behavior: contain;
		padding: var(--s0) var(--gutter) var(--s8);
	}
	.state {
		margin: var(--s0);
		padding: var(--s10) var(--s6);
		text-align: center;
	}
	.state-title {
		margin: var(--s0);
		color: var(--text);
		font-size: var(--fs-body);
	}
	.hint {
		margin: var(--s3) var(--s0) var(--s0);
		color: var(--muted);
		font-size: var(--fs-meta);
	}
	.retry {
		min-height: var(--target);
		margin-top: var(--s6);
		padding: var(--s4) var(--s8);
		border: var(--s0);
		border-radius: var(--radius-pill);
		background: var(--raised);
		color: var(--text);
	}
	.total {
		margin: var(--s2) var(--s4) var(--s3);
		color: var(--secondary);
		font-size: var(--fs-sm);
	}
	.hits {
		display: flex;
		flex-direction: column;
		gap: var(--s1);
		margin: var(--s0);
		padding: var(--s0);
		list-style: none;
	}
	.hit {
		display: flex;
		flex-direction: column;
		align-items: stretch;
		gap: var(--s2);
		width: 100%;
		min-height: var(--target);
		padding: var(--s4);
		border: var(--s0);
		border-radius: var(--radius);
		background: var(--transparent);
		color: var(--text);
		text-align: start;
		transition: background-color var(--duration-fast) var(--ease);
	}
	@media (hover: hover) and (pointer: fine) {
		.hit:hover {
			background: var(--hover);
		}
	}
	.hit:active {
		background: var(--selected);
	}
	.hit:focus-visible {
		outline: var(--focus-width) solid var(--accent);
		outline-offset: calc(var(--focus-width) * -1);
	}
	.head {
		display: flex;
		align-items: baseline;
		gap: var(--s4);
		min-width: 0;
	}
	.title {
		flex: 1;
		min-width: 0;
		overflow: hidden;
		color: var(--text);
		font-weight: var(--weight-semibold);
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.time {
		flex: none;
		color: var(--secondary);
		font-size: var(--fs-sm);
		font-variant-numeric: tabular-nums;
	}
	.meta {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: var(--s1) var(--s4);
		min-width: 0;
		color: var(--secondary);
		font-size: var(--fs-sm);
	}
	.origin {
		color: var(--accent-link);
	}
	.archived {
		color: var(--warning);
	}
	.who {
		min-width: 0;
		overflow: hidden;
		color: var(--secondary);
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.more-state {
		margin: var(--s0);
		padding: var(--s6);
		text-align: center;
	}
	.sentinel {
		height: var(--s1);
	}
	@media (prefers-reduced-motion: reduce) {
		.hit {
			transition: none;
		}
	}
</style>
