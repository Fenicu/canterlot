<script lang="ts">
	import { onMount } from 'svelte';
	import type { ChatStore } from '#lib/stores/chat.svelte.js';
	import type { components } from '#lib/api/generated/openapi.js';
	import { errorText } from '#lib/api/errors.js';
	import { conversationUsage } from '#lib/api/client.js';
	import { buildDiagnosticsReport, type DiagnosticsData } from './diagnostics';
	import { summarizeUsage, type UsageSection } from './usage';
	import {
		diagnosticEventLabel,
		diagnosticKindLabel,
		executionOutcomeLabel,
		executionStateLabel,
		modelLabel,
		scopeStatusLabel
	} from '#lib/format/labels.js';
	import Button from '#lib/ui/Button.svelte';
	import Sheet from '#lib/ui/Sheet.svelte';

	interface DiagnosticEventLike {
		kind?: string;
		type?: string;
		correlation_id?: string;
		payload?: { correlation_id?: string; event_type?: string; field_names?: string[] };
		seq?: string;
		created_seq?: string;
	}

	interface DetailLike {
		title?: string;
		machine?: { name?: string };
	}

	interface ActivityLike {
		state?: string;
		evidence?: Record<string, unknown>;
	}

	let {
		chat,
		onclose
	}: {
		chat: ChatStore;
		onclose: () => void;
	} = $props();

	let copied = $state(false);
	let usage = $state<components['schemas']['SessionUsageView'] | null>(null);
	let usageLoading = $state(true);
	let usageError = $state<string | null>(null);

	onMount(() => {
		let alive = true;
		void (async () => {
			try {
				const response = await conversationUsage(chat.id);
				if (alive) usage = response.data;
			} catch (error) {
				if (alive) usageError = errorText(error, 'Не удалось загрузить расход токенов');
			} finally {
				if (alive) usageLoading = false;
			}
		})();
		return () => {
			alive = false;
		};
	});

	const usageSummary = $derived(usage ? summarizeUsage(usage) : null);

	/** Число колонок таблицы: «Вход», «Выход» и кэш, если он есть. */
	function columns(section: UsageSection): number {
		return 2 + Number(section.showCacheRead) + Number(section.showCacheWrite);
	}

	const detail = $derived(chat.detail as DetailLike | null);
	const execution = $derived(chat.execution);
	const activity = $derived(chat.activity as ActivityLike | null);

	const diagnosticEvents = $derived(
		(chat.diagnostics ?? []).map((raw) => {
			const e = raw as DiagnosticEventLike;
			return {
				kind: e.kind || e.type || 'diagnostic',
				detail: e.payload?.event_type,
				fields: e.payload?.field_names,
				correlation_id: e.correlation_id || e.payload?.correlation_id,
				seq: e.seq || e.created_seq
			};
		})
	);

	const diagData = $derived<DiagnosticsData>({
		conversationId: chat.id,
		execution: {
			execution_id: execution?.execution_id,
			state: execution?.state ?? 'idle',
			generation: execution?.generation,
			outcome: execution?.outcome ?? null
		},
		activity: {
			state: activity?.state ?? 'idle',
			evidence: activity?.evidence ?? null
		},
		scope: {
			cursor: chat.cursor,
			status: chat.status,
			resyncCount: 0,
			lastFrameAgeMs: 0
		},
		versions: {
			build: '2026.10.29',
			protocol: 1
		},
		diagnosticEvents
	});

	async function copyReport() {
		const text = buildDiagnosticsReport(diagData);
		try {
			if (typeof navigator !== 'undefined' && navigator.clipboard) {
				await navigator.clipboard.writeText(text);
			}
		} catch {
			// Clipboard API might be restricted in some environments
		}
		copied = true;
		setTimeout(() => {
			copied = false;
		}, 3000);
	}
</script>

{#snippet modelsTable(section: UsageSection)}
	<table class="usage-table" aria-label="Токены по моделям">
		<thead>
			<tr>
				<th scope="col">Вход</th>
				<th scope="col">Выход</th>
				{#if section.showCacheRead}
					<th scope="col" aria-label="Кэш-чтение"><abbr title="Кэш-чтение">Кэш-чт.</abbr></th>
				{/if}
				{#if section.showCacheWrite}
					<th scope="col" aria-label="Кэш-запись"><abbr title="Кэш-запись">Кэш-зап.</abbr></th>
				{/if}
			</tr>
		</thead>
		{#each section.models as row (row.model)}
			<tbody>
				<tr>
					<th scope="rowgroup" colspan={columns(section)} class="model" title={row.model}>{modelLabel(row.model)}</th>
				</tr>
				<tr>
					<td title={row.input.exact}>{row.input.compact}</td>
					<td title={row.output.exact}>{row.output.compact}</td>
					{#if section.showCacheRead}<td title={row.cacheRead.exact}>{row.cacheRead.compact}</td>{/if}
					{#if section.showCacheWrite}<td title={row.cacheWrite.exact}>{row.cacheWrite.compact}</td>{/if}
				</tr>
			</tbody>
		{/each}
	</table>
{/snippet}

<Sheet open title="Диагностика" subtitle={`${detail?.title ?? 'Сессия'} · ${detail?.machine?.name ?? 'workstation'}`} {onclose}>
	<div class="diagnostics">
		<p class="note">Метаданные доставки, без текста сообщений.</p>

		<section data-role="session-usage">
			<h3>Токены за сессию</h3>
			{#if usageLoading && !usage}
				<p class="note">Загрузка…</p>
			{:else if usageError}
				<p class="note">{usageError}</p>
			{:else if usageSummary}
				<div class="usage">
					<div class="usage-group" data-role="usage-session">
						<div class="rows">
							{#each usageSummary.session.rows as row (row.label)}
								<div class="key-value">
									<span>{row.label}</span>
									<strong title={row.value.exact}>{row.value.compact}</strong>
								</div>
							{/each}
						</div>
						{#if usageSummary.session.models.length > 0}
							{@render modelsTable(usageSummary.session)}
						{/if}
					</div>

					<div class="usage-group" data-role="usage-delegates">
						<h4>Делегаты</h4>
						{#if !usageSummary.hasDelegates}
							<p class="note">Делегированных сессий нет.</p>
						{:else}
							<div class="rows">
								{#each usageSummary.delegates.rows as row (row.label)}
									<div class="key-value">
										<span>{row.label}</span>
										<strong title={row.value.exact}>{row.value.compact}</strong>
									</div>
								{/each}
							</div>
							{#if usageSummary.delegates.models.length > 0}
								{@render modelsTable(usageSummary.delegates)}
							{/if}
						{/if}
					</div>
				</div>
			{/if}
		</section>

		<section>
			<h3>Исполнение</h3>
			<div class="rows">
				<div class="key-value"><span>Состояние</span><code>{executionStateLabel(diagData.execution?.state ?? 'idle')}</code></div>
				<div class="key-value"><span>Поколение</span><code>{diagData.execution?.generation ?? 'нет'}</code></div>
				{#if diagData.execution?.outcome}
					<div class="key-value"><span>Итог</span><code>{executionOutcomeLabel(diagData.execution.outcome)}</code></div>
				{/if}
			</div>
		</section>

		<section>
			<h3>Журнал и соединение</h3>
			<div class="rows">
				<div class="key-value"><span>Сессия</span><code>{diagData.conversationId}</code></div>
				<div class="key-value"><span>Подписка</span><code>{scopeStatusLabel(diagData.scope?.status ?? 'idle')}</code></div>
				<div class="key-value"><span>Курсор</span><code>{diagData.scope?.cursor ?? 'нет'}</code></div>
			</div>
		</section>

		<section>
			<h3>Диагностические события</h3>
			{#if diagnosticEvents.length === 0}
				<p class="note">Событий сбоев или расхождений нет.</p>
			{:else}
				<div class="rows">
					{#each diagnosticEvents as ev, idx (idx)}
						<div class="key-value">
							<span class="event-kind">{diagnosticKindLabel(ev.kind)}</span>
							{#if ev.detail}<code>{diagnosticEventLabel(ev.detail) ?? ev.detail}</code>{/if}
							{#if ev.correlation_id}<code>{ev.correlation_id}</code>{/if}
							{#if ev.fields?.length}
								<span class="fields">Поля кадра: <code>{ev.fields.join(', ')}</code></span>
							{/if}
						</div>
					{/each}
				</div>
			{/if}
		</section>

		<Button variant="primary" full onclick={copyReport}>{copied ? 'Отчёт скопирован' : 'Скопировать отчёт'}</Button>
	</div>
</Sheet>

<style>
	.diagnostics {
		display: flex;
		flex-direction: column;
		gap: var(--s6);
	}
	.note {
		margin: 0;
		color: var(--muted);
		font-size: var(--fs-meta);
	}
	h3 {
		margin: 0 0 var(--s3);
		font-size: var(--fs-body);
		font-weight: var(--weight-semibold);
	}
	h4 {
		margin: var(--s3) 0;
		font-size: var(--fs-meta);
		font-weight: var(--weight-semibold);
		color: var(--secondary);
	}
	.usage {
		display: flex;
		flex-direction: column;
		gap: var(--s4);
	}
	.usage-group {
		display: flex;
		flex-direction: column;
		gap: var(--s2);
	}
	.usage-table {
		width: 100%;
		table-layout: fixed;
		border-collapse: separate;
		border-spacing: 0;
		font-size: var(--fs-meta);
		font-variant-numeric: tabular-nums;
	}
	/* Числовые колонки равной ширины: заголовок и значения стоят в одних столбцах. */
	.usage-table th,
	.usage-table td {
		padding: var(--s1) calc(var(--s3) / 2);
		text-align: right;
		white-space: nowrap;
	}
	.usage-table th:first-child,
	.usage-table td:first-child {
		padding-inline-start: 0;
	}
	.usage-table th:last-child,
	.usage-table td:last-child {
		padding-inline-end: 0;
	}
	.usage-table th {
		color: var(--muted);
		font-weight: normal;
	}
	.usage-table th[scope='col'] {
		padding-bottom: var(--s2);
	}
	.usage-table abbr {
		text-decoration: none;
	}
	/* Имя модели — отдельная строка во всю ширину, без моноширинного переноса по слогам. */
	.usage-table th.model {
		padding: var(--s4) 0 var(--s1);
		text-align: left;
		white-space: normal;
		overflow-wrap: anywhere;
		color: var(--text);
		font-weight: var(--weight-medium);
	}
	.rows {
		display: flex;
		flex-direction: column;
		gap: var(--s3);
		padding: var(--s5) var(--s6);
		border-radius: var(--radius-card);
		background: var(--raised);
	}
	.key-value {
		display: flex;
		flex-wrap: wrap;
		justify-content: space-between;
		gap: var(--s1) var(--s4);
		min-width: 0;
		font-size: var(--fs-meta);
	}
	.key-value span {
		color: var(--secondary);
	}
	.key-value code {
		min-width: 0;
		font-family: var(--font-mono);
		overflow-wrap: anywhere;
	}
	.key-value .fields {
		flex-basis: 100%;
		min-width: 0;
	}
	.event-kind {
		font-weight: var(--weight-semibold);
		color: var(--warning);
	}
</style>
