<script lang="ts">
	import { goto } from '$app/navigation';
	import { resolve } from '$app/paths';
	import { onMount } from 'svelte';
	import { browserRuntime } from './open';
	import { getRuntime, type Runtime } from '#lib/app/runtime.js';
	import { loadConfig } from '#lib/api/config.js';
	import Composer from '#lib/composer/Composer.svelte';
	import type { ChatStore } from '#lib/stores/chat.svelte.js';
	import PendingStrip from '#lib/timeline/PendingStrip.svelte';
	import Timeline from '#lib/timeline/Timeline.svelte';
	import ChatHeader from './ChatHeader.svelte';
	import PauseCards from './PauseCards.svelte';
	import TodoPanel from './TodoPanel.svelte';
	import RequestCard from '#lib/requests/RequestCard.svelte';
	import { isDocked } from '#lib/stores/requests.svelte.js';
	import SessionSettings from './SessionSettings.svelte';
	import Diagnostics from './Diagnostics.svelte';
	import InfoSheet from './InfoSheet.svelte';
	import DelegationSheet from './DelegationSheet.svelte';
	import WorktreeNotice from './WorktreeNotice.svelte';
	import ConfirmDialog from '#lib/ui/ConfirmDialog.svelte';
	import { ACCEPT_LABEL, acceptView, type AcceptDetail } from './accept';
	import { acceptSession } from './controls';
	import { errorText } from '#lib/api/errors.js';
	import SessionSheet from '#lib/screens/sessions/SessionSheet.svelte';
	import { childLive } from '#lib/screens/sessions/grouping.js';
	import { pane } from '#lib/shell/pane.svelte.js';
	import type { ProviderLimits, LimitThresholds } from '#lib/stores/limits.js';
	import type { MachineRecord } from '#lib/stores/machines.svelte.js';
	import Sheet from '#lib/ui/Sheet.svelte';

	interface MachineRef {
		id?: string;
	}

	interface ChatDetailWithMachine {
		machine?: MachineRef;
	}

	let {
		chat,
		onInfo,
		now = Date.now(),
		providers = [],
		thresholds = { warn_pcts: [80, 90], weekly_reserve_pct: 95, stale_after_secs: 1800 },
		machine = null
	}: {
		chat: ChatStore;
		onInfo?: (runId: string) => void;
		now?: number;
		providers?: ProviderLimits[];
		thresholds?: LimitThresholds;
		machine?: MachineRecord | null;
	} = $props();

	let runtime = $state<Runtime | null>(null);
	try {
		const rt = getRuntime();
		rt.machines.open();
		runtime = rt;
	} catch {
		// runtime singleton not yet created
	}

	onMount(() => {
		browserRuntime().then((rt) => {
			runtime = rt;
			rt.machines.open();
		});
	});

	// Лимиты берутся из хранилища списка на каждый рендер: копия при открытии застывала бы до выхода из чата.
	const limitProviders = $derived(providers.length > 0 ? providers : (runtime?.list.limits ?? []));
	const machineId = $derived((chat.detail as ChatDetailWithMachine | null)?.machine?.id);
	const resolvedMachine = $derived(
		machine ?? (runtime && machineId ? (runtime.machines.items.get(machineId) ?? null) : null)
	);

	let showSettings = $state(false);
	let showDiagnostics = $state(false);
	let showDelegation = $state(false);
	let showSheet = $state(false);
	const listRow = $derived(runtime?.list.items.get(chat.id) ?? null);
	const childSessions = $derived(
		runtime ? [...runtime.list.items.values()].filter((row) => row.parent_id === chat.id) : []
	);
	const childRunning = $derived(childSessions.filter(childLive).length);
	const parentTitle = $derived.by(() => {
		const parentId = (chat.detail as AcceptDetail | null)?.parent_id;
		return parentId ? (runtime?.list.items.get(parentId)?.title ?? null) : null;
	});
	const accept = $derived(acceptView(chat.detail as AcceptDetail | null));
	let showAccept = $state(false);
	let acceptError = $state<string | null>(null);

	async function runAccept(force: boolean) {
		acceptError = null;
		try {
			await acceptSession(chat.id, { force });
		} catch (err: unknown) {
			acceptError = errorText(err, 'Не удалось принять результат');
		}
	}
	let infoRunId = $state<string | null>(null);

	onMount(async () => {
		try {
			const runtime = await browserRuntime();
			const detail = chat.detail as ChatDetailWithMachine | null;
			if (!machine && detail?.machine?.id) {
				const mid = detail.machine.id;
				machine = runtime.machines.items.get(mid) ?? [...runtime.machines.items.values()][0] ?? null;
			}
			const cfg = await loadConfig();
			if (cfg?.limit_thresholds) {
				thresholds = cfg.limit_thresholds;
			}
		} catch {
			// fallback
		}
	});

	const waitingId = $props.id();
	const docked = $derived([...chat.requests.items.values()].filter(isDocked));
	// Вопрос из активности без открытого запроса отвечать некуда: показываем его только для чтения.
	const waitingQuestion = $derived.by((): string | null => {
		const evidence = chat.activity?.evidence as Record<string, unknown> | undefined;
		if (chat.activity?.state === 'waiting_for_user' && typeof evidence?.question === 'string') {
			return evidence.question;
		}
		return null;
	});
</script>

<section class="screen" data-ready>
	<ChatHeader
		{chat}
		providers={limitProviders}
		{thresholds}
		{now}
		onOpenSheet={listRow ? () => (showSheet = true) : undefined}
		onOpenSettings={() => {
			if (pane.desktop) pane.open('settings');
			else showSettings = true;
		}}
		onOpenDiagnostics={() => (showDiagnostics = true)}
		onOpenDelegation={() => (showDelegation = true)}
		onAccept={() => {
			acceptError = null;
			showAccept = true;
		}}
		{parentTitle}
		childRunning={childRunning}
		hasChildren={childSessions.length > 0}
	/>

	<Timeline
		{chat}
		onInfo={(runId) => {
			infoRunId = runId;
			onInfo?.(runId);
		}}
		{now}
	/>

	<TodoPanel {chat} />

	{#if docked.length > 0}
		<div class="request-cards-slot">
			{#each docked as req (req.id)}
				<RequestCard
					request={req}
					onDecision={async (decision) => {
						await chat.requests.submitDecision(req.id, decision, chat.ops);
					}}
				/>
			{/each}
		</div>
	{:else if waitingQuestion}
		<div class="request-card-slot">
			<section class="waiting-notice" aria-labelledby={waitingId}>
				<h2 class="waiting-title" id={waitingId}>Агент ждёт ответа</h2>
				<p class="waiting-question">{waitingQuestion}</p>
			</section>
		</div>
	{/if}

	{#if accept.dirty}
		<WorktreeNotice error={acceptError} onforce={() => (showAccept = true)} />
	{:else if acceptError}
		<p class="accept-error" role="alert">{acceptError}</p>
	{/if}

	<PauseCards {chat} {now} />

	<PendingStrip {chat} />

	<Composer {chat} />

	{#if showAccept}
		<ConfirmDialog
			title={ACCEPT_LABEL}
			body={accept.body}
			confirmLabel={accept.confirmLabel}
			danger
			onconfirm={() => {
				showAccept = false;
				void runAccept(accept.force);
			}}
			onclose={() => (showAccept = false)}
		/>
	{/if}

	{#if showSettings}
		<Sheet open title="Настройки сессии" onclose={() => (showSettings = false)}>
			<SessionSettings
				{chat}
				machine={resolvedMachine}
				onclose={() => (showSettings = false)}
			/>
		</Sheet>
	{/if}

	{#if showSheet && listRow}
		<SessionSheet
			item={listRow}
			groups={runtime?.list.groups ?? []}
			machine={resolvedMachine}
			touch
			onclose={() => (showSheet = false)}
			ondeleted={() => void goto(resolve('sessions'))}
		/>
	{/if}

	{#if showDiagnostics}
		<Diagnostics
			{chat}
			onclose={() => (showDiagnostics = false)}
		/>
	{/if}

	{#if showDelegation && !pane.desktop}
		<DelegationSheet {chat} onclose={() => (showDelegation = false)} />
	{/if}

	{#if infoRunId}
		<InfoSheet
			{chat}
			runId={infoRunId}
			onclose={() => (infoRunId = null)}
		/>
	{/if}
</section>

<style>
	.screen {
		flex: 1;
		height: auto;
		display: flex;
		flex-direction: column;
		min-width: 0;
		min-height: 0;
		position: relative;
	}

	.screen > :global(:not(dialog)) {
		width: 100%;
		max-width: var(--reading-max);
		margin-inline: auto;
	}

	.screen > :global(.feed-wrap) {
		max-width: none;
	}

	.waiting-notice {
		display: flex;
		flex-direction: column;
		gap: var(--s2);
		padding: var(--s4);
		border-radius: var(--radius-card);
		background: var(--surface);
		color: var(--text);
	}

	.waiting-title {
		margin: var(--s0);
		font-size: var(--fs-meta);
		font-weight: var(--weight-medium);
		color: var(--muted);
	}

	.waiting-question {
		margin: var(--s0);
		font-size: var(--fs-body);
		overflow-wrap: anywhere;
	}

	.accept-error {
		margin: var(--s0) var(--gutter) var(--s3);
		font-size: var(--fs-meta);
		color: var(--danger);
	}
	.request-cards-slot,
	.request-card-slot {
		padding: var(--s3) var(--gutter);
		box-sizing: border-box;
		width: 100%;
		display: flex;
		flex-direction: column;
		gap: var(--s3);
	}

	/* Слот ограничивает высоту карточек, а прокручивается каждая карточка сама: так
	   закреплённая кнопка ответа ложится на её скруглённый низ. 60dvh ограничивает
	   слот на обычном экране, calc оставляет место шапке и полю ввода, когда
	   клавиатура сжимает видимую область. */
	.request-cards-slot {
		flex: 0 1 auto;
		min-height: 0;
		max-height: var(--request-card-max);
	}
</style>
