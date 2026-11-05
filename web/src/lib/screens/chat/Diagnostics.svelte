<script lang="ts">
	import { onMount } from 'svelte';
	import type { ChatStore } from '#lib/stores/chat.svelte.js';
	import type { components } from '#lib/api/generated/openapi.js';
	import { errorText } from '#lib/api/errors.js';
	import { conversationUsage } from '#lib/api/client.js';
	import { buildDiagnosticsReport, type DiagnosticsData } from './diagnostics';
	import { summarizeUsage } from './usage';
	import {
		diagnosticEventLabel,
		diagnosticKindLabel,
		executionOutcomeLabel,
		executionStateLabel,
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
							<table class="usage-table">
								<thead>
									<tr>
										<th>Модель</th>
										<th>Вход</th>
										<th>Выход</th>
										{#if usageSummary.session.showCacheRead}<th>Кэш-чтение</th>{/if}
										{#if usageSummary.session.showCacheWrite}<th>Кэш-запись</th>{/if}
									</tr>
								</thead>
								<tbody>
									{#each usageSummary.session.models as row (row.model)}
										<tr>
											<td><code>{row.model}</code></td>
											<td title={row.input.exact}>{row.input.compact}</td>
											<td title={row.output.exact}>{row.output.compact}</td>
											{#if usageSummary.session.showCacheRead}<td title={row.cacheRead.exact}>{row.cacheRead.compact}</td>{/if}
											{#if usageSummary.session.showCacheWrite}<td title={row.cacheWrite.exact}>{row.cacheWrite.compact}</td>{/if}
										</tr>
									{/each}
								</tbody>
							</table>
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
								<table class="usage-table">
									<thead>
										<tr>
											<th>Модель</th>
											<th>Вход</th>
											<th>Выход</th>
											{#if usageSummary.delegates.showCacheRead}<th>Кэш-чтение</th>{/if}
											{#if usageSummary.delegates.showCacheWrite}<th>Кэш-запись</th>{/if}
										</tr>
									</thead>
									<tbody>
										{#each usageSummary.delegates.models as row (row.model)}
											<tr>
												<td><code>{row.model}</code></td>
												<td title={row.input.exact}>{row.input.compact}</td>
												<td title={row.output.exact}>{row.output.compact}</td>
												{#if usageSummary.delegates.showCacheRead}<td title={row.cacheRead.exact}>{row.cacheRead.compact}</td>{/if}
												{#if usageSummary.delegates.showCacheWrite}<td title={row.cacheWrite.exact}>{row.cacheWrite.compact}</td>{/if}
											</tr>
										{/each}
									</tbody>
								</table>
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
		border-collapse: collapse;
		font-size: var(--fs-meta);
		font-variant-numeric: tabular-nums;
	}
	.usage-table th,
	.usage-table td {
		padding: var(--s1) 0;
		text-align: right;
		white-space: nowrap;
	}
	.usage-table th:first-child,
	.usage-table td:first-child {
		text-align: left;
		padding-right: var(--s3);
		white-space: normal;
	}
	.usage-table th {
		color: var(--muted);
		font-weight: normal;
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
