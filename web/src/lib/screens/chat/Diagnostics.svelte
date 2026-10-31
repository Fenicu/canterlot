<script lang="ts">
	import type { ChatStore } from '#lib/stores/chat.svelte.js';
	import { buildDiagnosticsReport, type DiagnosticsData } from './diagnostics';
	import {
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
		payload?: { correlation_id?: string; event_type?: string };
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
		onclose
	}: {
		chat: ChatStore;
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
				detail: e.payload?.event_type,
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

		<section>
			<h3>Исполнение</h3>
			<div class="rows">
				<div class="key-value"><span>Состояние</span><code>{executionStateLabel(diagData.execution?.state ?? 'idle')}</code></div>
				<div class="key-value"><span>Поколение</span><code>{diagData.execution?.generation ?? 1}</code></div>
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
							{#if ev.detail}<code>{ev.detail}</code>{/if}
							{#if ev.correlation_id}<code>{ev.correlation_id}</code>{/if}
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
	.event-kind {
		font-weight: var(--weight-semibold);
		color: var(--warning);
	}
</style>
