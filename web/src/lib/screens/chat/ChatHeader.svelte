<script lang="ts">
	import { errorText } from '#lib/api/errors.js';
	import { resolve } from '$app/paths';
	import type { ChatStore } from '#lib/stores/chat.svelte.js';
	import type { InterruptControl } from '#lib/composer/interrupt.svelte.js';
	import type { Activity } from '#lib/screens/sessions/activityText.js';
	import StateChip from './StateChip.svelte';
	import { chatStateLine, effectiveActivity } from './stateLine';
	import { criticalLimit } from './criticalLimit';
	import { modelAliasNote, modelLabel, resolvedModelLabel } from '#lib/format/labels.js';
	import { fmt } from '#lib/format/ru.js';
	import Icon from '#lib/ui/Icon.svelte';
	import { dismissable, type DismissReason } from '#lib/ui/dismiss.js';
	import { pane } from '#lib/shell/pane.svelte.js';
	import Menu from '#lib/ui/Menu.svelte';
	import MenuItem from '#lib/ui/MenuItem.svelte';
	import { stateMark } from '#lib/screens/sessions/rowState.js';
	import { theme } from '#lib/theme.js';
	import { stopSession, resumeSession, continueSessionQueue, resolveOwnerLost, controlBusy, clearSnooze } from './controls';
	import { RESOLVE_WARNING, runLabel } from './pauseCards';
	import { goalState, goalUsage } from './goalView';
	import type { ProviderLimits } from '#lib/stores/limits.js';
	import { limitThresholds } from '#lib/stores/limitThresholds.svelte.js';
	import { runningCount } from '#lib/stores/delegation.js';
	import { liveNodes } from './delegationNodes';
	import { isSubagentCall } from '#lib/timeline/rows.js';
	import { ACCEPT_LABEL, acceptView, type AcceptDetail } from './accept';
	import { nudgeStateText } from '#lib/timeline/nudge.js';
	import { asDecision } from '#lib/routing/decision.js';
	import { routingTitles } from '#lib/routing/titles.svelte.js';
	import { getRuntime } from '#lib/app/runtime.js';
	import { pendingItems } from '#lib/routing/queue.js';
	import { waitFactsFromRequest } from '#lib/requests/waitCopy.js';
	import { WAITING_SECTION, focusListSection, otherWaitingCount } from '#lib/screens/sessions/waiting.js';
	import SnoozeMenu from '#lib/screens/sessions/SnoozeMenu.svelte';
	import MuteMenu from '#lib/screens/sessions/MuteMenu.svelte';
	import { limitResetAt } from '#lib/screens/sessions/snooze.js';

	interface DetailLike {
		title?: string;
		color?: string | null;
		harness?: { id?: string; title?: string; provider?: string; goals?: boolean };
		machine?: { name?: string };
		model?: string;
		model_resolved?: string | null;
		snoozed_until?: string | null;
		notifications_muted?: boolean;
	}

	interface SettingsLike {
		effective?: { model?: string };
		pending?: { model?: string };
	}

	/** Только то, что нужно тихому уведомлению о простое машины. */
	interface MachineLike {
		availability?: string;
		availability_since?: string | null;
	}

	let {
		chat,
		interrupt = null,
		providers = [],
		now = Date.now(),
		machine = null,
		onOpenSheet,
		onOpenSettings,
		onOpenDiagnostics,
		onOpenDelegation,
		onOpenMessages,
		onOpenSearch,
		onOpenSide,
		sideHint = null,
		onOpenBookmarks,
		bookmarkCount = 0,
		onOpenAlarms,
		onOpenNudge,
		onOpenGoal,
		onOpenRoute,
		onAccept,
		parentTitle = null,
		childRunning = 0,
		hasChildren = false
	}: {
		chat: ChatStore;
		providers?: ProviderLimits[];
		now?: number;
		machine?: MachineLike | null;
		onOpenSheet?: () => void;
		onOpenSettings?: () => void;
		onOpenDiagnostics?: () => void;
		onOpenDelegation?: () => void;
		onOpenMessages?: () => void;
		onOpenSearch?: () => void;
		/** Вход в боковой чат; `sideHint` делает его недоступным с подсказкой. */
		onOpenSide?: () => void;
		sideHint?: string | null;
		/** Избранное разговора: лист с закладками и их число. */
		onOpenBookmarks?: () => void;
		bookmarkCount?: number;
		onOpenAlarms?: () => void;
		onOpenNudge?: () => void;
		/** Детали цели: только у харнессов, объявивших поддержку. */
		onOpenGoal?: () => void;
		/** Решение маршрута, выбравшее исполнителя этой сессии. */
		onOpenRoute?: () => void;
		onAccept?: () => void;
		/** Название сессии, запустившей эту; пусто, пока список её не знает. */
		parentTitle?: string | null;
		/** Живых делегированных сессий: идут в счётчик делегирования. */
		childRunning?: number;
		/** У сессии есть делегированные: вход в делегирование виден и без субагентов. */
		hasChildren?: boolean;
		/** Прерывание идущего хода; есть, только пока ход идёт. */
		interrupt?: InterruptControl | null;
	} = $props();

	let menuOpen = $state(false);
	const delegated = $derived(hasChildren || pendingItems(chat.spawnQueue).length > 0 || chat.timeline.entries.some((entry) => entry.kind === 'block' && isSubagentCall(entry.block)));
	const alarmCount = $derived(
		chat.alarms.filter((alarm) => alarm.state === 'armed' || alarm.state === 'failed').length
	);
	const running = $derived(delegated ? runningCount(liveNodes(chat)) + childRunning : 0);
	let confirmingForce = $state(false);
	let actionError = $state<string | null>(null);

	async function handleUnsnooze() {
		actionError = null;
		try {
			await clearSnooze(chat.id);
		} catch (err) {
			actionError = errorText(err, 'Не удалось вернуть сессию');
		}
	}

	const detail = $derived(chat.detail as DetailLike | null);
	const parentId = $derived(((chat.detail as AcceptDetail | null)?.parent_id) ?? null);
	const accept = $derived(acceptView(chat.detail as AcceptDetail | null));
	const settings = $derived(chat.settings as SettingsLike | null);
	// Служебный разговор облачных карточек: хаб запрещает в нём ходы, поэтому действия
	// запуска и настройки модели здесь только обещают недоступное.
	const systemChat = $derived(detail?.harness?.id === 'cloud');

	// Другие сессии, которые ждут владельца прямо сейчас: счёт живёт на списке и меняется событиями.
	const waitingOthers = $derived.by(() => {
		try {
			return otherWaitingCount(getRuntime().list.items.values(), chat.id);
		} catch {
			return 0;
		}
	});
	const waitingTitle = $derived(
		waitingOthers > 0
			? `Ждут ответа: ${waitingOthers} ${fmt.plural(waitingOthers, ['сессия', 'сессии', 'сессий'])}`
			: ''
	);

	const title = $derived(detail?.title || 'Сессия');
	// Тихая строка «Отложена до …»: срок в прошлом значит, что сессия уже вернулась.
	const snoozedAt = $derived.by(() => {
		const parsed = detail?.snoozed_until ? Date.parse(detail.snoozed_until) : Number.NaN;
		return Number.isFinite(parsed) && parsed > now ? parsed : null;
	});
	// Открытый запрос говорит в шапке своим видом и возрастом, а не общей фразой.
	const pending = $derived.by(() => {
		const stamp = (value?: string): number => {
			const parsed = Date.parse(value ?? '');
			return Number.isFinite(parsed) ? parsed : Number.POSITIVE_INFINITY;
		};
		const open = [...chat.requests.items.values()]
			.filter((request) => request.state === 'open' || request.state === 'sending')
			.sort((a, b) => stamp(a.createdAt) - stamp(b.createdAt));
		const request = open[0];
		return request ? waitFactsFromRequest(request.kind, request.payload, request.createdAt) : null;
	});
	// Знак шапки по тем же правилам, что у строки списка: кольцо, точка или серый кружок без процесса.
	const mark = $derived(stateMark(effectiveActivity(chat.activity as Activity | null, chat.phase, (chat.execution as { state?: string })?.state)));
	// Та же строка, что у знака состояния: полный текст уходит в подсказку, на экране — короткие слова.
	const stateLine = $derived(chatStateLine(
		chat.activity as Activity | null,
		chat.phase,
		(chat.execution as { state?: string })?.state,
		now,
		pending
	));
	const harnessTitle = $derived(detail?.harness?.title || 'Claude Code');
	const machineName = $derived(detail?.machine?.name || 'workstation');
	const provider = $derived(detail?.harness?.provider || 'claude');
	const nudgeText = $derived(chat.nudge ? nudgeStateText(chat.nudge) : '');
	// Лимиты в чате молчат, пока окно провайдера этой сессии не дошло до тревожного порога.
	const limitAlert = $derived(criticalLimit(providers, provider, now, limitThresholds));
	const goalSupported = $derived(!!detail?.harness?.goals);
	const goal = $derived(goalSupported ? chat.goal : null);
	const goalStateView = $derived(goal ? goalState(goal.status) : null);
	const goalUsageText = $derived(goal ? goalUsage(goal) : '');
	const route = $derived(asDecision((chat.detail as { routing?: unknown } | null)?.routing));
	const routeTitle = $derived(route ? routingTitles.title(route.task_type) : '');
	$effect(() => {
		if (!route) return;
		let revision: number | null = null;
		try {
			revision = getRuntime().list.routingRevision;
		} catch {
			revision = null;
		}
		routingTitles.ensure(revision);
	});

	const requestedModel = $derived(settings?.effective?.model || detail?.model || 'opus-5.5');
	// Конкретная версия относится к модели сессии: после смены модели её ещё не знают.
	const resolvedModel = $derived(detail?.model === requestedModel ? (detail?.model_resolved ?? null) : null);
	const effectiveModel = $derived(resolvedModelLabel(requestedModel, resolvedModel));
	const aliasNote = $derived(modelAliasNote(requestedModel, resolvedModel));
	const pendingModel = $derived(settings?.pending?.model && modelLabel(settings.pending.model));

	const subtitle = $derived(
		systemChat
			? `${harnessTitle} · ${machineName}`
			: pendingModel
				? `${harnessTitle} · ${effectiveModel} → ${pendingModel} со следующего хода · ${machineName}`
				: `${harnessTitle} · ${effectiveModel} · ${machineName}`
	);

	const metaTitle = $derived(
		[stateLine.quiet ? '' : stateLine.text, subtitle, aliasNote].filter(Boolean).join('\n')
	);

	// Тихая строка вместо погашенного состояния: сколько машина уже не на связи.
	const machineDown = $derived.by(() => {
		if (machine?.availability !== 'unavailable') return '';
		const since = machine.availability_since ? Date.parse(machine.availability_since) : Number.NaN;
		if (!Number.isFinite(since)) return 'Машина недоступна';
		const down = Math.max(0, now - since);
		return down >= 60_000 ? `Машина недоступна ${fmt.durationCoarse(down)}` : 'Машина недоступна';
	});

	const isLive = $derived(chat.execution
		? (chat.execution as { live?: boolean; state?: string }).live ?? ['created', 'starting', 'running', 'stopping'].includes((chat.execution as { state?: string }).state ?? '')
		: false);

	const stopping = $derived(controlBusy(`stop:${chat.id}:false`));
	const hasUserStop = $derived(chat.queue.pause_reasons.some((p) => (p as { reason?: string })?.reason === 'user_stop'));
	const ownerLost = $derived((chat.execution as { state?: string } | null)?.state === 'owner_lost');

	async function handleStop(force = false) {
		actionError = null;
		try {
			const { data } = await stopSession(chat.id, { force });
			confirmingForce = false;
			if (data === 'owner_lost') {
				actionError = 'Связь с исполнением потеряна: остановить его можно только действием «Завершить исполнение»';
				return;
			}
			menuOpen = false;
		} catch (err: unknown) {
			actionError = errorText(err, 'Не удалось остановить сессию');
		}
	}

	async function handleResolve() {
		actionError = null;
		try {
			await resolveOwnerLost(chat.id);
			menuOpen = false;
			confirmingForce = false;
		} catch (err: unknown) {
			actionError = errorText(err, 'Не удалось завершить исполнение');
		}
	}

	async function handleRun() {
		actionError = null;
		try {
			await resumeSession(chat.id, { continue_queue: chat.queue.pause_reasons.length > 0 });
			menuOpen = false;
		} catch (err: unknown) {
			actionError = errorText(err, 'Не удалось запустить сессию');
		}
	}

	async function handleContinueQueue() {
		actionError = null;
		try {
			await continueSessionQueue(chat.id);
			menuOpen = false;
		} catch (err: unknown) {
			actionError = errorText(err, 'Не удалось продолжить очередь');
		}
	}

	let menuButton = $state<HTMLButtonElement | null>(null);

	function closeMenu(reason: DismissReason) {
		menuOpen = false;
		confirmingForce = false;
		if (reason === 'escape') menuButton?.focus();
	}
</script>


<div class="chat-header" role="region" aria-label="Шапка чата" data-top-chrome>
	<div class="topbar" class:tall={!!goal} class:context={!!goal}>
		{#if !pane.desktop}
			<a
				class="icon-button back"
				class:waiting={waitingOthers > 0}
				href={resolve('sessions')}
				aria-label={waitingOthers > 0 ? `Назад, ждут ответа: ${waitingOthers}` : 'Все сессии'}
				title={waitingTitle || undefined}
				data-back-waiting={waitingOthers > 0 ? waitingOthers : undefined}
				onclick={() => { if (waitingOthers > 0) focusListSection(WAITING_SECTION); }}
			><Icon name="back" />{#if waitingOthers > 0}<span class="back-count" aria-hidden="true">{waitingOthers}</span>{/if}</a>
		{/if}

		<div class="header-info">
			<div class="title-row">
				{#if detail}
					<span class="mark" data-state={mark.state} data-halo={mark.halo} class:dashed={mark.dashed} class:dim={mark.dim} title={mark.word}><span class="sr-only">{mark.word}</span></span>
					<h1 class="header-title" {title}>{title}</h1>
				{:else}
					<span class="skeleton" style:width="var(--bar-56)"></span>
				{/if}
			</div>
			<!-- Одна строка: короткое состояние акцентом, затем агент, модель и машина. Полный текст — в подсказке. -->
			<div class="meta-row" data-role="header-meta" title={metaTitle} data-model-alias={aliasNote || undefined}>
				{#if detail}
					<StateChip
						activity={chat.activity as Activity | null}
						phase={chat.phase}
						executionState={(chat.execution as { state?: string })?.state}
						now={now}
						{pending}
						short
					/>
					<span class="subtitle">{#if !stateLine.quiet}<span class="meta-sep" aria-hidden="true">&nbsp;·&nbsp;</span>{/if}{subtitle}</span>
				{:else}
					<span class="skeleton" style:width="var(--bar-74)"></span>
				{/if}
			</div>
			{#if goal && goalStateView}
				<button type="button" class="goal-row" data-role="goal-state-line" data-tone={goalStateView.tone}
					aria-label={`Цель: ${goal.objective}. ${goalStateView.text}`} onclick={onOpenGoal}>
					<span class="goal-word">{goalStateView.text}</span>
					<span class="goal-objective">{goal.objective}</span>
					{#if goalUsageText}<span class="goal-usage">{goalUsageText}</span>{/if}
				</button>
			{/if}
		</div>

		<div class="header-actions">
			<button
				type="button"
				class="icon-button messages-btn"
				aria-label="Мои сообщения"
				aria-pressed={pane.desktop ? pane.view === 'messages' : undefined}
				onclick={() => {
					if (pane.desktop) pane.toggle('messages');
					else onOpenMessages?.();
				}}
			>
				<Icon name="list" />
			</button>
			{#if pane.desktop && delegated}
				<button type="button" class="pane-action" aria-pressed={pane.view === 'delegation'} onclick={() => pane.toggleDelegation()}>{running > 0 ? `Делегирование · ${running}` : 'Делегирование'}</button>
			{/if}
			{#if !pane.desktop && delegated && onOpenDelegation}
				<button
					type="button"
					class="icon-button delegation-btn"
					class:busy={running > 0}
					aria-label={running > 0 ? `Делегирование, работает ${running}` : 'Делегирование'}
					onclick={onOpenDelegation}
				>
					<Icon name="branch" />
					{#if running > 0}<span class="running-count">{running}</span>{/if}
				</button>
			{/if}
			<button
				bind:this={menuButton}
				type="button"
				class="icon-button menu-btn"
				aria-label="Меню сессии"
				aria-haspopup="true"
				aria-expanded={menuOpen}
				onclick={() => { menuOpen = !menuOpen; confirmingForce = false; }}
			>
				<Icon name="more" />
			</button>

			<!-- На телефоне тема уходит в меню сессии: так названию остаётся место. -->
			{#if pane.desktop}
				<button
					type="button"
					class="icon-button theme-toggle-btn"
					data-theme-toggle
					aria-label="Сменить тему"
					onclick={() => theme.toggle()}
				>
					<Icon name={theme.effective === 'dark' ? 'sun' : 'moon'} />
				</button>
			{/if}
		</div>
	</div>

	{#if snoozedAt !== null}
		<p class="header-status snoozed-line" role="status">
			<span>Отложена до {fmt.clock(snoozedAt)}</span>
			<button type="button" class="unsnooze" onclick={() => void handleUnsnooze()}>Вернуть</button>
		</p>
	{/if}

	{#if detail?.notifications_muted}
		<p class="header-status muted-line" role="status" data-notifications-muted>
			<Icon name="bellOff" />
			<span>Без уведомлений</span>
		</p>
	{/if}

	{#if stopping}
		<p class="header-status" role="status">Останавливаю сессию</p>
	{:else if actionError && !menuOpen}
		<p class="header-status header-error" role="alert">{actionError}</p>
	{:else if machineDown}
		<p class="header-status machine-down" data-machine-down>{machineDown}</p>
	{/if}

	{#if limitAlert}
		<p class="header-status limit-line" role="status">
			<a class="limit-alert" data-role="limit-alert" href={resolve('limits')}>
				<Icon name="warning" />
				<span class="limit-text">{limitAlert.text}</span>
			</a>
		</p>
	{/if}

	{#if menuOpen}
		<div class="session-dropdown" use:dismissable={{ onclose: closeMenu, keep: (target) => menuButton?.contains(target) ?? false }}>
			<Menu label="Действия над сессией">
				{#if actionError}
					<div class="action-error" role="alert">{actionError}</div>
				{/if}

				<!-- Откуда сессия и как её ведут: родитель, маршрут и автопинок ушли сюда из шапки. -->
				{#if parentId}
					<!-- Стрелка вверх — к сессии, которая запустила эту; ветка остаётся у маршрута, булавка — у названия. -->
				<MenuItem icon="send" href={resolve(`c/${parentId}`)} onclick={() => { menuOpen = false; }}>
						<span data-role="menu-parent">Запущена сессией {parentTitle || 'без названия'}</span>
					</MenuItem>
				{/if}
				{#if route && onOpenRoute}
					<MenuItem icon="branch" onclick={() => { menuOpen = false; onOpenRoute(); }}>
						<span data-role="menu-route">Маршрут: {routeTitle}</span>
					</MenuItem>
				{/if}
				{#if nudgeText && onOpenNudge}
					<!-- Автопинок ждёт простоя и толкает агента: песочные часы, круговая стрелка занята очередью. -->
					<MenuItem icon="hourglass" onclick={() => { menuOpen = false; onOpenNudge(); }}>
						<span data-role="menu-nudge">{nudgeText}</span>
					</MenuItem>
				{/if}
				{#if parentId || (route && onOpenRoute) || (nudgeText && onOpenNudge)}<div class="menu-divider" role="separator"></div>{/if}

				{#if onOpenSheet}
					<MenuItem icon="pin" onclick={() => { menuOpen = false; onOpenSheet(); }}>Название, цвет и группы</MenuItem>
				{/if}

				{#if onOpenSide && !systemChat}
					<MenuItem
						icon="chats"
						disabled={Boolean(sideHint)}
						title={sideHint ?? undefined}
						onclick={() => {
							if (sideHint) return;
							menuOpen = false;
							onOpenSide();
						}}>Спросить в стороне</MenuItem
					>
				{/if}

				{#if onOpenSettings && !systemChat}
					<MenuItem icon="sliders" onclick={() => { menuOpen = false; onOpenSettings(); }}>Настройки сессии</MenuItem>
				{/if}

				{#if onOpenSearch}
					<MenuItem icon="search" onclick={() => { menuOpen = false; onOpenSearch(); }}>Искать в этой сессии</MenuItem>
				{/if}

				{#if onOpenBookmarks}
					<MenuItem icon="bookmark" onclick={() => { menuOpen = false; onOpenBookmarks(); }}>Избранное{bookmarkCount > 0 ? ` · ${bookmarkCount}` : ''}</MenuItem>
				{/if}

				{#if onOpenDiagnostics}
					<MenuItem icon="info" onclick={() => { menuOpen = false; onOpenDiagnostics(); }}>Диагностика</MenuItem>
				{/if}

				<SnoozeMenu
					conversationId={chat.id}
					snoozed={snoozedAt !== null}
					resetAt={limitResetAt(chat.queue)}
					now={now}
					ondone={() => { menuOpen = false; }}
					onerror={(err) => { actionError = errorText(err, 'Не удалось отложить сессию'); }}
				/>

				<MuteMenu
					conversationId={chat.id}
					muted={Boolean(detail?.notifications_muted)}
					ondone={() => { menuOpen = false; }}
					onerror={(err) => { actionError = errorText(err, 'Не удалось изменить уведомления'); }}
				/>

				{#if alarmCount > 0 && onOpenAlarms}
					<MenuItem
						icon="clock"
						onclick={() => {
							menuOpen = false;
							if (pane.desktop) pane.toggle('alarms');
							else onOpenAlarms();
						}}
					>Будильники · {alarmCount}</MenuItem>
				{/if}

				{#if !pane.desktop}
					<MenuItem icon={theme.effective === 'dark' ? 'sun' : 'moon'} onclick={() => { menuOpen = false; theme.toggle(); }}>{theme.effective === 'dark' ? 'Светлая тема' : 'Тёмная тема'}</MenuItem>
				{/if}

				{#if !systemChat}<div class="menu-divider" role="separator"></div>{/if}

				{#if interrupt}
					<MenuItem icon="stop" onclick={() => { menuOpen = false; void interrupt.interrupt(); }}>{interrupt.running ? interrupt.label : 'Прервать ход'}</MenuItem>
				{/if}

				{#if accept.available && onAccept}
					<MenuItem icon="trash" onclick={() => { menuOpen = false; onAccept(); }}>{ACCEPT_LABEL}</MenuItem>
				{/if}

				{#if !systemChat}
					{#if isLive}
						<MenuItem icon="stop" onclick={() => { if (!stopping) void handleStop(false); }}>{stopping ? 'Останавливаю сессию' : 'Остановить сессию'}</MenuItem>
						{#if !confirmingForce}
							<MenuItem icon="warning" danger onclick={() => { confirmingForce = true; }}>Принудительно остановить</MenuItem>
						{:else}
							<div class="confirm-box" role="dialog" aria-label="Подтверждение остановки">
								<p>Принудительно прервать выполнение?</p>
								<div class="confirm-actions">
									<button type="button" class="btn-confirm-danger" onclick={() => void handleStop(true)}>
										Остановить принудительно
									</button>
									<button type="button" class="btn-cancel" onclick={() => { confirmingForce = false; }}>
										Отмена
									</button>
								</div>
							</div>
						{/if}
					{:else if ownerLost}
						{#if !confirmingForce}
							<MenuItem icon="warning" danger onclick={() => { confirmingForce = true; }}>Завершить исполнение</MenuItem>
						{:else}
							<div class="confirm-box" role="dialog" aria-label="Подтверждение завершения">
								<p>{RESOLVE_WARNING}</p>
								<div class="confirm-actions">
									<button type="button" class="btn-confirm-danger" onclick={() => void handleResolve()}>
										Завершить
									</button>
									<button type="button" class="btn-cancel" onclick={() => { confirmingForce = false; }}>
										Отмена
									</button>
								</div>
							</div>
						{/if}
					{:else}
						<MenuItem icon="play" onclick={() => void handleRun()}>{runLabel(chat.turnRecorded)}</MenuItem>
					{/if}

					{#if isLive && hasUserStop}
						<MenuItem icon="retry" onclick={() => void handleContinueQueue()}>Продолжить очередь</MenuItem>
					{/if}
				{/if}
			</Menu>
		</div>
	{/if}
</div>

<style>
	.chat-header {
		position: relative;
		display: flex;
		flex-direction: column;
		z-index: 5;
	}
	.topbar {
		display: flex;
		align-items: center;
		gap: var(--s2);
		min-width: 0;
		padding-block: var(--s2);
		margin: var(--s5) var(--gutter) var(--s3);
		min-height: var(--target);
	}
	/* Под ссылкой на родителя кнопки остаются у названия и состояния, а не посередине трёх строк. */
	.topbar.tall {
		align-items: flex-start;
	}
	/* Цель строки цели растёт вниз: оставляем ей место, чтобы касание не ложилось на ленту. */
	.topbar.context {
		margin-bottom: calc(var(--s10) + var(--s1));
	}
	.header-info {
		flex: 1;
		display: flex;
		flex-direction: column;
		/* Название и подпись вместе укладываются в строку 44 px: шаг 3 px держит разрывы ровными,
		   а лишние пиксели шапки отнимали высоту у прижатой ленты. */
		gap: calc(var(--s2) - var(--hairline));
		min-width: 0;
	}
	.title-row {
		display: flex;
		align-items: center;
		gap: var(--s3);
		min-width: 0;
	}
	.header-title {
		margin: 0;
		min-width: 0;
		font-size: var(--fs-title);
		font-weight: var(--weight-semibold);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
		color: var(--text);
	}
	.mark {
		--mark-color: var(--success);
		width: var(--mark-large);
		height: var(--mark-large);
		box-sizing: border-box;
		border-radius: var(--radius-round);
		flex-shrink: 0;
		background: var(--mark-color);
		background-clip: content-box;
	}
	/* Кольцо — идёт ход, ждёт владельца, беда или пауза; точка — процесс жив; полый серый кружок — процесса нет.
	   Кольцо живёт внутри тех же рамок, что и точка: контур снаружи съедал зазор до названия. */
	.mark[data-halo] {
		padding: calc(var(--s1) + var(--hairline));
		outline: var(--s1) solid var(--mark-color);
		outline-offset: calc(var(--s1) * -1);
	}
	.mark[data-halo='--accent'] { --mark-color: var(--accent); }
	.mark[data-halo='--warning'] { --mark-color: var(--warning); }
	.mark[data-halo='--danger'] { --mark-color: var(--danger); }
	.mark.dashed { outline-style: dashed; }
	.mark[data-state='fresh'] { --mark-color: var(--muted); }
	.mark.dim { background: var(--transparent); border: var(--s1) solid var(--muted); }
	/* Усыплённая сессия: полый холодный кружок, процесс не жив. */
	.mark[data-state='hibernated'].dim { border-color: var(--blue); }
	/* Состояние и подпись делят одну строку: состояние держит свои слова, подпись режется многоточием. */
	.meta-row {
		display: flex;
		align-items: center;
		min-width: 0;
		padding-left: calc(var(--mark-large) + var(--s3));
		font-size: var(--fs-sm);
		line-height: var(--lh-tight);
		white-space: nowrap;
	}
	.subtitle {
		flex: 1 1 0;
		min-width: 0;
		color: var(--muted);
		overflow: hidden;
		text-overflow: ellipsis;
	}
	/* Лимит на исходе — одна строка тревоги; касание ведёт на экран лимитов. */
	.limit-line {
		margin-top: calc(var(--s3) * -1);
		margin-bottom: var(--s0);
	}
	.limit-alert {
		display: flex;
		align-items: center;
		gap: var(--s2);
		min-width: 0;
		min-height: var(--target);
		color: var(--danger);
	}
	.limit-alert :global(svg) {
		flex: none;
		width: var(--icon-small);
		height: var(--icon-small);
	}
	.limit-text {
		min-width: 0;
		overflow: hidden;
		white-space: nowrap;
		text-overflow: ellipsis;
	}
	.limit-alert:focus-visible {
		outline: var(--focus-width) solid var(--accent);
	}
	.header-actions {
		display: flex;
		flex: none;
		align-items: center;
		gap: var(--s1);
		margin-right: var(--icon-hang);
	}
	.back {
		margin-left: var(--icon-hang);
	}
	/* Ожидание в других сессиях: янтарный счёт рядом со стрелкой, мишень шире на цифру. */
	.back.waiting {
		width: auto;
		min-width: var(--target);
		gap: var(--s1);
		padding-inline: var(--s2);
		color: var(--warning);
	}
	.back-count {
		font-size: var(--fs-meta);
		font-weight: var(--weight-bold);
		font-variant-numeric: tabular-nums;
	}
	.icon-button {
		background: var(--transparent);
		border: none;
		color: var(--text);
		cursor: pointer;
		display: flex;
		align-items: center;
		justify-content: center;
		flex: none;
		width: var(--target);
		height: var(--target);
		border-radius: var(--radius-round);
	}
	.icon-button[aria-pressed='true'] {
		background: var(--selected);
		color: var(--accent-link);
	}
	.header-status {
		margin: 0 var(--gutter) var(--s2);
		font-size: var(--fs-meta);
		color: var(--muted);
	}
	.header-error {
		color: var(--danger);
	}
	/* Отложенная сессия говорит тихо: срок и возврат в одной строке. */
	.snoozed-line { display: flex; align-items: center; gap: var(--s3); flex-wrap: wrap; }
	.muted-line { display: flex; align-items: center; gap: var(--s3); }
	.muted-line :global(svg) { width: var(--icon-small); height: var(--icon-small); color: var(--muted); }
	.unsnooze {
		min-height: var(--target);
		padding-inline: var(--s3);
		border-radius: var(--radius-sm);
		background: var(--transparent);
		color: var(--accent-link);
		font-size: var(--fs-meta);
	}
	.delegation-btn {
		width: auto;
		min-width: var(--target);
		gap: var(--s1);
		padding-inline: var(--s2);
	}
	.delegation-btn.busy {
		color: var(--accent-link);
	}
	.running-count {
		font-size: var(--fs-sm);
		font-variant-numeric: tabular-nums;
	}
	.pane-action {
		white-space: nowrap;
		min-height: var(--target);
		padding-inline: var(--s8);
		border-radius: var(--radius-pill);
		background: var(--transparent);
		color: var(--secondary);
		font-size: var(--fs-meta);
		transition: background-color var(--duration-fast) var(--ease);
	}
	.pane-action[aria-pressed='true'] {
		background: var(--selected);
		color: var(--accent-link);
	}
	.session-dropdown {
		position: absolute;
		right: var(--gutter);
		top: calc(100% - var(--s2));
		z-index: 20;
		max-width: calc(100% - 2 * var(--gutter));
	}
	.menu-divider {
		height: var(--s4);
	}
	.confirm-box {
		padding: var(--s4);
		border-radius: var(--radius-sm);
		background: var(--danger-soft);
		font-size: var(--fs-meta);
	}
	.confirm-box p {
		margin: 0 0 var(--s4);
	}
	.confirm-actions {
		display: flex;
		flex-wrap: wrap;
		gap: var(--s2);
	}
	.btn-confirm-danger, .btn-cancel {
		white-space: nowrap;
		min-height: var(--target);
		padding: var(--s2) var(--s6);
		border-radius: var(--radius-pill);
		font-size: var(--fs-meta);
	}
	.btn-confirm-danger {
		background: var(--danger);
		color: var(--bg);
	}
	.btn-cancel {
		background: var(--transparent);
		color: var(--text);
	}
	.action-error {
		padding: var(--s2) var(--s5);
		font-size: var(--fs-meta);
		color: var(--danger);
	}
	/* Цель — та же тихая строка, что маршрут и автопинок: объектив режется одной
	   строкой, статус окрашивается словом без значков, 44 px цели растут вниз. */
	.goal-row {
		position: relative;
		display: grid;
		grid-template-columns: auto minmax(0, 1fr) auto;
		align-items: center;
		gap: var(--s2);
		min-width: 0;
		max-width: 100%;
		margin-top: var(--s1);
		padding: var(--s0);
		border: var(--s0);
		background: transparent;
		font-size: var(--fs-sm);
		line-height: var(--lh-tight);
		text-align: left;
	}
	.goal-row::after {
		content: '';
		position: absolute;
		left: var(--s0);
		right: var(--s0);
		top: var(--s0);
		height: var(--target);
	}
	.goal-word {
		color: var(--muted);
		white-space: nowrap;
	}
	.goal-row[data-tone='success'] .goal-word { color: var(--success); }
	.goal-row[data-tone='warning'] .goal-word { color: var(--warning); }
	.goal-row[data-tone='danger'] .goal-word { color: var(--danger); }
	.goal-objective {
		overflow: hidden;
		white-space: nowrap;
		text-overflow: ellipsis;
		color: var(--text);
	}
	.goal-usage {
		color: var(--muted);
		white-space: nowrap;
	}
	.goal-row:hover .goal-objective,
	.goal-row:focus-visible .goal-objective {
		text-decoration: underline;
		text-underline-offset: var(--s1);
	}
</style>
