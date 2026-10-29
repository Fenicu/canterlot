<script lang="ts">
	import type { ChatStore } from '$lib/stores/chat.svelte';
	import { buildDiagnosticsReport, type DiagnosticsData } from './diagnostics';
	import Sheet from '$lib/ui/Sheet.svelte';

	interface DiagnosticEventLike {
		kind?: string;
		type?: string;
		correlation_id?: string;
		payload?: { correlation_id?: string };
		seq?: string;
		created_seq?: string;
	}

	interface DetailLike {
		title?: string;
		connection_id?: string;
		machine?: { name?: string };
	}

	interface ExecutionLike {
		id?: string;
		state?: string;
		generation?: number;
		outcome?: string | null;
		live?: boolean;
	}

	interface ActivityLike {
		state?: string;
		evidence?: Record<string, unknown>;
	}

	let {
		chat,
		touch = true,
		onclose
	}: {
		chat: ChatStore;
		touch?: boolean;
		onclose: () => void;
	} = $props();

	let copied = $state(false);

	const detail = $derived(chat.detail as DetailLike | null);
	const execution = $derived(chat.execution as ExecutionLike | null);
	const activity = $derived(chat.activity as ActivityLike | null);

	const diagnosticEvents = $derived(
		(chat.diagnostics ?? []).map((raw) => {
			const e = raw as DiagnosticEventLike;
			return {
				kind: e.kind || e.type || 'diagnostic',
				correlation_id: e.correlation_id || e.payload?.correlation_id,
				seq: e.seq || e.created_seq
			};
		})
	);

	const diagData = $derived<DiagnosticsData>({
		conversationId: chat.id,
		connectionId: detail?.connection_id ?? 'conn_live',
		execution: {
			id: execution?.id,
			state: execution?.state ?? (execution?.live ? 'live' : 'idle'),
			generation: execution?.generation ?? 1,
			outcome: execution?.outcome ?? null
		},
		activity: {
			state: activity?.state ?? 'idle',
			evidence: activity?.evidence ?? null
		},
		scope: {
			cursor: 'journal:live',
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

{#snippet content()}
	<div class="diagnostics-sheet">
		<header class="diag-header">
			<div>
				<h2>Диагностика</h2>
				<div class="subtitle">
					{detail?.title ?? 'Сессия'} · {detail?.machine?.name ?? 'workstation'}
				</div>
			</div>
			<button type="button" class="icon-button" aria-label="Закрыть" onclick={onclose}>
				<svg viewBox="0 0 24 24" aria-hidden="true"><path d="m6 6 12 12M6 18 18 6" /></svg>
			</button>
		</header>

		<div class="diag-body scroll">
			<p class="diag-note">Метаданные доставки, без текста сообщений.</p>

			<section class="diag-section">
				<h3>Исполнение</h3>
				<div class="diag-rows">
					<div class="key-value"><span>Состояние</span><code>{diagData.execution?.state ?? 'idle'}</code></div>
					<div class="key-value"><span>Поколение</span><code>{diagData.execution?.generation ?? 1}</code></div>
					{#if diagData.execution?.outcome}
						<div class="key-value"><span>Итог</span><code>{diagData.execution.outcome}</code></div>
					{/if}
				</div>
			</section>

			<section class="diag-section">
				<h3>Журнал и соединение</h3>
				<div class="diag-rows">
					<div class="key-value"><span>Сессия</span><code>{diagData.conversationId}</code></div>
					<div class="key-value"><span>Статус scope</span><code>{diagData.scope?.status ?? 'idle'}</code></div>
					<div class="key-value"><span>Курсор</span><code>{diagData.scope?.cursor ?? 'none'}</code></div>
				</div>
			</section>

			<section class="diag-section">
				<h3>Диагностические события</h3>
				{#if diagnosticEvents.length === 0}
					<p class="meta">Событий сбоев или расхождений нет.</p>
				{:else}
					<div class="diag-events-list">
						{#each diagnosticEvents as ev, idx (idx)}
							<div class="diag-event-item">
								<span class="event-kind">{ev.kind}</span>
								{#if ev.correlation_id}
									<code class="event-corr">{ev.correlation_id}</code>
								{/if}
							</div>
						{/each}
					</div>
				{/if}
			</section>
		</div>

		<footer class="diag-footer">
			<button type="button" class="button primary full" onclick={copyReport}>
				{copied ? 'Отчёт скопирован' : 'Скопировать отчёт'}
			</button>
		</footer>
	</div>
{/snippet}

{#if touch}
	<Sheet open title="Диагностика" {onclose}>
		{@render content()}
	</Sheet>
{:else}
	<div class="popover-wrapper">
		{@render content()}
	</div>
{/if}

<style>
	.diagnostics-sheet {
		display: flex;
		flex-direction: column;
		height: 100%;
		background: var(--surface);
		color: var(--text);
		box-sizing: border-box;
		padding: var(--s4);
	}
	.popover-wrapper {
		position: absolute;
		z-index: 10;
		right: var(--s4);
		top: var(--s4);
		width: min(100%, var(--phone-max));
		max-height: 85vh;
		border-radius: var(--radius-card);
		background: var(--surface);
		box-shadow: var(--shadow);
		border: var(--hairline) solid var(--line);
		overflow: hidden;
	}
	.diag-header {
		display: flex;
		justify-content: space-between;
		align-items: center;
		margin-bottom: var(--s4);
	}
	.diag-header h2 {
		margin: 0;
		font-size: var(--fs-title);
		font-weight: var(--weight-semibold);
	}
	.subtitle {
		font-size: var(--fs-meta);
		color: var(--muted);
	}
	.diag-body {
		flex: 1;
		display: flex;
		flex-direction: column;
		gap: var(--s4);
		overflow-y: auto;
	}
	.diag-note {
		margin: 0;
		font-size: var(--fs-meta);
		color: var(--muted);
	}
	.diag-section h3 {
		margin: 0 0 var(--s2);
		font-size: var(--fs-body);
		font-weight: var(--weight-semibold);
	}
	.diag-rows {
		display: flex;
		flex-direction: column;
		gap: var(--s2);
		padding: var(--s3);
		border-radius: var(--radius-card);
		background: var(--raised);
	}
	.key-value {
		display: flex;
		justify-content: space-between;
		font-size: var(--fs-meta);
	}
	.key-value code {
		font-family: var(--font-mono);
	}
	.diag-events-list {
		display: flex;
		flex-direction: column;
		gap: var(--s2);
	}
	.diag-event-item {
		display: flex;
		justify-content: space-between;
		padding: var(--s2);
		border-radius: var(--radius-card);
		border: var(--hairline) solid var(--line);
		font-size: var(--fs-meta);
	}
	.event-kind {
		font-weight: var(--weight-semibold);
		color: var(--warning);
	}
	.diag-footer {
		padding-top: var(--s4);
		margin-top: auto;
	}
	.button.primary {
		padding: var(--s3);
		border-radius: var(--radius-card);
		background: var(--accent);
		color: var(--accent-ink);
		border: none;
		font-weight: var(--weight-semibold);
		cursor: pointer;
		text-align: center;
	}
	.button.full {
		width: 100%;
	}
	.icon-button {
		background: var(--transparent);
		border: none;
		color: var(--text);
		cursor: pointer;
		display: flex;
		align-items: center;
		justify-content: center;
		width: var(--target);
		height: var(--target);
	}
	.icon-button svg {
		width: var(--icon);
		height: var(--icon);
		fill: none;
		stroke: currentColor;
		stroke-width: 2;
	}
</style>
