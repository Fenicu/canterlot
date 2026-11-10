<script lang="ts">
	import { openSearch } from '#lib/search/open.svelte.js';
	import { goto } from '$app/navigation';
	import { resolve } from '$app/paths';
	import { page } from '$app/state';
	import { onMount, untrack } from 'svelte';
	import { browserRuntime } from '#lib/app/browser.js';
	import { getRuntime, type Runtime } from '#lib/app/runtime.js';
	import { loadConfig } from '#lib/api/config.js';
	import Composer from '#lib/composer/Composer.svelte';
	import { InterruptControl } from '#lib/composer/interrupt.svelte.js';
	import { turnIsLive } from '#lib/composer/sendRules.js';
	import type { ChatStore, OwnerInputView } from '#lib/stores/chat.svelte.js';
	import PendingStrip from '#lib/timeline/PendingStrip.svelte';
	import { sessionBookmarks } from '#lib/bookmarks/store.svelte.js';
	import Timeline from '#lib/timeline/Timeline.svelte';
	import ChatHeader from './ChatHeader.svelte';
	import PauseCards from './PauseCards.svelte';
	import SpawnQueueStrip from './SpawnQueueStrip.svelte';
	import PlanStrip from '#lib/screens/plan/PlanStrip.svelte';
	import RequestCard from '#lib/requests/RequestCard.svelte';
	import { isDocked } from '#lib/stores/requests.svelte.js';
	import DelegationSheet from './DelegationSheet.svelte';
	import WorktreeNotice from './WorktreeNotice.svelte';
	import ConfirmDialog from '#lib/ui/ConfirmDialog.svelte';
	import { ACCEPT_LABEL, acceptView, type AcceptDetail } from './accept';
	import { acceptSession } from './controls';
	import { errorText } from '#lib/api/errors.js';
	import { fmt } from '#lib/format/ru.js';
	import SessionSheet from '#lib/screens/sessions/SessionSheet.svelte';
	import { childLive } from '#lib/screens/sessions/grouping.js';
	import { pane } from '#lib/shell/pane.svelte.js';
	import type { ProviderLimits } from '#lib/stores/limits.js';
	import type { MachineRecord } from '#lib/stores/machines.svelte.js';
	import Sheet from '#lib/ui/Sheet.svelte';
	import type { Component } from 'svelte';

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
		machine = null
	}: {
		chat: ChatStore;
		onInfo?: (runId: string) => void;
		now?: number;
		providers?: ProviderLimits[];
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

	// Прервать ход можно из живой строки хода и из меню сессии: у них одно состояние.
	const interrupt = untrack(() => new InterruptControl({ conversationId: chat.id }));
	const turn = $derived(turnIsLive(chat.activity, chat.queue.run_slot));
	let armReset = false;
	$effect(() => {
		const status = chat.interrupt && typeof chat.interrupt.status === 'string' ? chat.interrupt.status : '';
		if (status) interrupt.applyStatus(status);
	});
	$effect(() => {
		if (!turn) armReset = true;
		else if (armReset && !interrupt.running) {
			armReset = false;
			interrupt.reset();
		}
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
	let showMessages = $state(false);
	let showAlarms = $state(false);
	let showNudge = $state(false);
	let showRoute = $state(false);
	let showSheet = $state(false);
	let infoRunId = $state<string | null>(null);
	// Редкие панели не входят в маршрут чата: подгружаются, когда их открывают.
	let TodoPanel = $state<Component<{ chat: ChatStore; lane?: string | null }> | null>(null);
	let SessionSettings = $state<Component<{ chat: ChatStore; machine?: MachineRecord | null; desktop?: boolean; onclose?: () => void }> | null>(null);
	let Diagnostics = $state<Component<{ chat: ChatStore; onclose: () => void }> | null>(null);
	let InfoSheet = $state<Component<{ chat: ChatStore; runId: string; onclose: () => void }> | null>(null);
	let OwnerMessagesSheet = $state<Component<{ chat: ChatStore; onpick: (item: OwnerInputView) => void; onclose: () => void }> | null>(null);
	let AlarmsSheet = $state<Component<{ chat: ChatStore; onclose: () => void }> | null>(null);
	let NudgeSheet = $state<Component<{ chat: ChatStore; onclose: () => void }> | null>(null);
	let RouteSheet = $state<Component<{ chat: ChatStore; onclose: () => void }> | null>(null);
	const todos = $derived(chat.todos.lane(null));
	// Закладки живут, пока открыт разговор: при уходе набор снимается вместе с подпиской ленты.
	const marks = $derived(sessionBookmarks.get(chat.id));
	const bookmarkError = $derived(marks?.toggleError ?? null);
	$effect(() => {
		const id = chat.id;
		untrack(() => sessionBookmarks.open(id));
		return () => sessionBookmarks.close(id);
	});

	$effect(() => {
		if (showSettings && !SessionSettings) import('./SessionSettings.svelte').then((m) => { SessionSettings = m.default; });
	});
	$effect(() => {
		if (showDiagnostics && !Diagnostics) import('./Diagnostics.svelte').then((m) => { Diagnostics = m.default; });
	});
	$effect(() => {
		if (infoRunId && !InfoSheet) import('./InfoSheet.svelte').then((m) => { InfoSheet = m.default; });
	});
	$effect(() => {
		if (showMessages && !OwnerMessagesSheet) import('./OwnerMessagesSheet.svelte').then((m) => { OwnerMessagesSheet = m.default; });
	});
	$effect(() => {
		if (showAlarms && !AlarmsSheet) import('./AlarmsSheet.svelte').then((m) => { AlarmsSheet = m.default; });
		if (showNudge && !NudgeSheet) import('./NudgeSheet.svelte').then((m) => { NudgeSheet = m.default; });
		if (showRoute && !RouteSheet) import('./RouteSheet.svelte').then((m) => { RouteSheet = m.default; });
	});
	// Список задач показывается не в каждом ходу: панель догружается вместе с ним.
	$effect(() => {
		if (todos && todos.items.length > 0 && !TodoPanel) import('./TodoPanel.svelte').then((m) => { TodoPanel = m.default; });
	});
	const listRow = $derived(runtime?.list.items.get(chat.id) ?? null);
	const childSessions = $derived(
		runtime ? [...runtime.list.items.values()].filter((row) => row.parent_id === chat.id) : []
	);
	const childRunning = $derived(childSessions.filter(childLive).length);
	// Задачи плана живут в списке владельца: вкладка «План» открывается в панели делегирования.
	const hasPlan = $derived(runtime?.list.plan.has(chat.id) ?? false);
	$effect.pre(() => pane.forChat(chat.id));
	function openPlan(): void {
		pane.openPlan();
		if (pane.desktop) pane.open('delegation');
		else showDelegation = true;
	}
	const parentTitle = $derived.by(() => {
		const parentId = (chat.detail as AcceptDetail | null)?.parent_id;
		return parentId ? (runtime?.list.items.get(parentId)?.title ?? null) : null;
	});
	const accept = $derived(acceptView(chat.detail as AcceptDetail | null));
	const sessionHarnessId = $derived((chat.detail?.harness as { id?: string } | undefined)?.id ?? '');
	const continuedFromAt = $derived(
		(chat.detail as { continued_from_at?: string | null } | null)?.continued_from_at ?? null
	);
	const sessionPermissionMode = $derived(
		(chat.settings.effective as { permission_mode?: string } | null | undefined)?.permission_mode ??
			((chat.detail?.permission_mode as string | undefined) ?? '')
	);
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

	onMount(async () => {
		try {
			const runtime = await browserRuntime();
			const detail = chat.detail as ChatDetailWithMachine | null;
			if (!machine && detail?.machine?.id) {
				const mid = detail.machine.id;
				machine = runtime.machines.items.get(mid) ?? [...runtime.machines.items.values()][0] ?? null;
			}
			// Конфиг обновляет общий реактивный порог для полосы лимитов.
			await loadConfig();
		} catch {
			// fallback
		}
	});

	// Ссылка «Отчёт» из плана и совпадение поиска приводят к месту в ленте:
	// `/c/<id>?block=<block_id>&seq=<n>` или `?input=<input_id>&seq=<n>`; старые ссылки без seq ищут блок по страницам.
	const jumpLink = $derived.by(() => {
		const params = page.url.searchParams;
		const block = params.get('block');
		const input = params.get('input');
		const seq = params.get('seq') ?? '';
		if (block) return { key: `block:${block}:${seq}`, block, input: '', seq };
		if (input && seq) return { key: `input:${input}:${seq}`, block: '', input, seq };
		return null;
	});
	// Прыжок ждёт снимка: до него ленты нет, и искать блок негде. Каждая ссылка — один раз за показ.
	let jumpedTo: string | null = null;
	$effect(() => {
		const link = jumpLink;
		if (!link || !chat.detail || jumpedTo === link.key) return;
		jumpedTo = link.key;
		untrack(() => {
			if (link.block) chat.requestBlockJump(link.block, link.seq);
			else chat.requestJump(link.input, link.seq);
		});
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
		interrupt={turn ? interrupt : null}
		providers={limitProviders}
		{now}
		machine={resolvedMachine}
		onOpenSheet={listRow ? () => (showSheet = true) : undefined}
		onOpenSettings={() => {
			if (pane.desktop) pane.open('settings');
			else showSettings = true;
		}}
		onOpenDiagnostics={() => (showDiagnostics = true)}
		onOpenDelegation={() => {
			pane.showDelegationTree();
			showDelegation = true;
		}}
		onOpenMessages={() => (showMessages = true)}
		onOpenSearch={() => openSearch('', { id: chat.id, title: (chat.detail as { title?: string } | null)?.title ?? '' })}
		onOpenAlarms={() => (showAlarms = true)}
		onOpenNudge={() => (showNudge = true)}
		onOpenRoute={() => (showRoute = true)}
		onAccept={() => {
			acceptError = null;
			showAccept = true;
		}}
		{parentTitle}
		childRunning={childRunning}
		hasChildren={childSessions.length > 0 || hasPlan}
	/>
	{#if continuedFromAt}
		<p class="continuation" data-role="continuation">Продолжение истории Claude Code от {fmt.stamp(Date.parse(continuedFromAt), now)}</p>
	{/if}

	<Timeline
		{chat}
		{interrupt}
		onInfo={(runId) => {
			infoRunId = runId;
			onInfo?.(runId);
		}}
		{now}
		bookmarks={marks}
		onOpenPlan={(id) => {
			if (id !== chat.id || !hasPlan) return false;
			openPlan();
			return true;
		}}
	/>

	{#if TodoPanel}<TodoPanel {chat} />{/if}

	{#if docked.length > 0}
		<div class="request-cards-slot">
			{#each docked as req (req.id)}
				<RequestCard
					request={req}
					arrive
					harnessId={sessionHarnessId}
					permissionMode={sessionPermissionMode}
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
	{#if bookmarkError}
		<p class="bookmark-error" role="alert">{bookmarkError}</p>
	{/if}

	<PauseCards {chat} {now} />

	<SpawnQueueStrip {chat} />

	{#if hasPlan}<PlanStrip orchestratorId={chat.id} onopen={openPlan} />{/if}

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

	{#if showSettings && SessionSettings}
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
			ontrashed={(id) => { sessionStorage.setItem('canterlot.trash-toast', id); window.dispatchEvent(new CustomEvent('canterlot:trash', { detail: id })); void goto(resolve('sessions')); }}
		/>
	{/if}

	{#if showDiagnostics && Diagnostics}
		<Diagnostics
			{chat}
			onclose={() => (showDiagnostics = false)}
		/>
	{/if}

	{#if showDelegation && !pane.desktop}
		<DelegationSheet {chat} onclose={() => (showDelegation = false)} />
	{/if}

	{#if showMessages && !pane.desktop && OwnerMessagesSheet}
		<OwnerMessagesSheet
			{chat}
			onclose={() => (showMessages = false)}
			onpick={(item) => {
				showMessages = false;
				chat.requestJump(item.id, item.seq);
			}}
		/>
	{/if}

	{#if showAlarms && !pane.desktop && AlarmsSheet}
		<AlarmsSheet {chat} onclose={() => (showAlarms = false)} />
	{/if}

	{#if showNudge && NudgeSheet}
		<NudgeSheet {chat} onclose={() => (showNudge = false)} />
	{/if}

	{#if showRoute && RouteSheet}
		<RouteSheet {chat} onclose={() => (showRoute = false)} />
	{/if}

	{#if infoRunId && InfoSheet}
		<InfoSheet
			{chat}
			runId={infoRunId}
			onclose={() => (infoRunId = null)}
		/>
	{/if}
</section>

<style>
	.bookmark-error {
		margin: 0 var(--s6);
		color: var(--danger);
		font-size: var(--fs-sm);
	}
	.continuation {
		margin: 0;
		padding: 0 var(--s6);
		color: var(--muted);
		font-size: var(--fs-sm);
	}
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
