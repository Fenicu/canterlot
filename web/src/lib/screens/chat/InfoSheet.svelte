<script lang="ts">
	import { errorText } from '#lib/api/errors.js';
	import { onMount } from 'svelte';
	import type { ChatStore } from '#lib/stores/chat.svelte.js';
	import { getRun } from '#lib/api/client.js';
	import type { RunInfo } from '#lib/stores/run.js';
	import { summarizeRun } from './info';
	import ExportActions from './ExportActions.svelte';
	import Sheet from '#lib/ui/Sheet.svelte';

	let {
		chat,
		runId,
		onclose
	}: {
		chat: ChatStore;
		runId: string;
		onclose: () => void;
	} = $props();

	let info = $state<RunInfo | null>(null);
	let loading = $state(true);
	let error = $state<string | null>(null);

	const sessionModel = $derived(
		((chat.settings as { effective?: { model?: string } } | null)?.effective?.model ||
			(chat.detail as { model?: string } | null)?.model) ?? null
	);
	const summary = $derived(info ? summarizeRun(info, chat.detail?.harness as { title?: string; provider?: string } | undefined, sessionModel) : null);

	// Collect blocks for export that belong to this run
	const runBlocks = $derived.by(() => {
		const list = [...chat.blocks.values()];
		return list.filter((b) => b.inputCommandId === runId || b.id === runId);
	});

	async function loadRunInfo() {
		try {
			const res = await getRun(chat.id, runId);
			info = res.data as RunInfo;
			loading = false;
		} catch (err: unknown) {
			error = errorText(err, 'Не удалось загрузить сведения о ходе');
			loading = false;
		}
	}

	onMount(() => {
		void loadRunInfo();

		// Listen for run_finished event to reload live
		const unsubscribe = chat.timeline?.entries ? () => {} : () => {};
		// We can also poll or observe chat events:
		const interval = setInterval(() => {
			if (info && !info.finished_at) {
				void loadRunInfo();
			}
		}, 1000);

		return () => {
			unsubscribe();
			clearInterval(interval);
		};
	});

	function formatThousands(n: number): string {
		if (n >= 1000) {
			const k = Math.round(n / 1000);
			return `${k} тыс.`;
		}
		return n.toLocaleString('ru-RU');
	}

	function formatTokens(n: number): string {
		return n.toLocaleString('ru-RU');
	}
</script>

<Sheet open title="Об ответе" {onclose}>
	<div class="info-sheet-body">
		{#if loading && !info}
			<p class="meta">Загрузка сведений…</p>
		{:else if error}
			<p class="error-msg">{error}</p>
		{:else if summary}
			<div class="info-header-meta">
				<p class="meta">
					{#if summary.pending}
						Ответ ещё поступает
					{:else}
						Ход завершён
					{/if}
				</p>
			</div>

			{#if summary.parts.length > 0}
				<h3>Модели по частям</h3>
				<table class="data-table">
					<thead>
						<tr>
							<th>Часть ответа</th>
							<th>Модель</th>
						</tr>
					</thead>
					<tbody>
						{#each summary.parts as part (part.title + part.model)}
							<tr>
								<td>
									{part.title}
									{#if part.sub}
										<span class="sub">{part.sub}</span>
									{/if}
								</td>
								<td><code>{part.model}</code></td>
							</tr>
						{/each}
					</tbody>
				</table>
			{/if}

			<div class="section-heading">
				Токены
				{#if summary.tokens.some((t) => t.sub !== null)}
					<span class="meta">основной + субагент</span>
				{/if}
			</div>

			{#if summary.pending && summary.tokens.every((t) => t.main === 0 && (t.sub === null || t.sub === 0))}
				<p class="meta">Токены появятся после завершения хода.</p>
			{:else}
				<div class="key-value-list">
					{#each summary.tokens as token (token.label)}
						<div class="key-value">
							<span>{token.label}</span>
							<strong>
								{#if token.sub !== null}
									{formatTokens(token.main)} + {formatTokens(token.sub)}
								{:else}
									{formatTokens(token.main)}
								{/if}
							</strong>
						</div>
					{/each}
				</div>
			{/if}

			<div class="rule"></div>

			{#if summary.duration}
				<div class="key-value">
					<span>Длительность</span>
					<strong>{summary.duration}</strong>
				</div>
			{/if}

			{#if summary.speed.length > 0}
				<div class="key-value speed" data-role="run-speed">
					<span>Скорость</span>
					<strong>
						{#each summary.speed as line (line)}<span class="speed-line">{line}</span>{/each}
					</strong>
				</div>
			{/if}

			{#if summary.context}
				<div class="key-value">
					<span>Контекст</span>
					<strong>{formatThousands(summary.context.used)} из {formatThousands(summary.context.window)}</strong>
				</div>
			{/if}

			{#if summary.limit}
				<div class="key-value">
					<span>Потрачено лимита</span>
					<strong>{summary.limit.text}</strong>
				</div>
				{#if summary.limit.note}
					<p class="meta">{summary.limit.note}</p>
				{/if}
			{/if}

			<ExportActions blocks={runBlocks} />
		{/if}
	</div>
</Sheet>

<style>
	.info-sheet-body {
		display: flex;
		flex-direction: column;
		gap: var(--s3);
		padding: var(--s2) 0;
	}
	.info-header-meta {
		margin-top: calc(-1 * var(--s2));
	}
	.meta {
		font-size: var(--fs-meta);
		color: var(--muted);
		margin: 0;
	}
	h3 {
		margin: var(--s2) 0 var(--s1);
		font-size: var(--fs-body);
		font-weight: var(--weight-semibold);
	}
	.section-heading {
		font-size: var(--fs-body);
		font-weight: var(--weight-semibold);
		margin-top: var(--s2);
		display: flex;
		align-items: baseline;
		gap: var(--s2);
	}
	.data-table {
		width: 100%;
		border-collapse: collapse;
		font-size: var(--fs-body);
	}
	.data-table th, .data-table td {
		text-align: left;
		padding: var(--s3) 0;
	}
	.data-table th {
		color: var(--muted);
		font-weight: normal;
		font-size: var(--fs-meta);
	}
	.sub {
		display: block;
		font-size: var(--fs-meta);
		color: var(--muted);
	}
	code {
		font-family: var(--font-mono);
		font-size: var(--fs-meta);
	}
	.key-value-list {
		display: flex;
		flex-direction: column;
		gap: var(--s2);
	}
	.key-value {
		display: flex;
		justify-content: space-between;
		align-items: baseline;
		font-size: var(--fs-body);
	}
	.key-value strong {
		font-variant-numeric: tabular-nums;
	}
	.speed {
		gap: var(--s3);
	}
	.speed strong {
		text-align: end;
	}
	.speed-line {
		display: block;
	}
	.rule {
		height: var(--s4);
	}
	.error-msg {
		color: var(--danger);
		font-size: var(--fs-meta);
	}
</style>
