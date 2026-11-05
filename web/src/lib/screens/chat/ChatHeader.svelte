<script lang="ts">
	import { errorText } from '#lib/api/errors.js';
	import { resolve } from '$app/paths';
	import type { ChatStore } from '#lib/stores/chat.svelte.js';
	import type { Activity } from '#lib/screens/sessions/activityText.js';
	import StateChip from './StateChip.svelte';
	import { chatStateLine } from './stateLine';
	import { modelLabel } from '#lib/format/labels.js';
	import { fmt } from '#lib/format/ru.js';
	import Icon from '#lib/ui/Icon.svelte';
	import { dismissable, type DismissReason } from '#lib/ui/dismiss.js';
	import { pane } from '#lib/shell/pane.svelte.js';
	import Menu from '#lib/ui/Menu.svelte';
	import MenuItem from '#lib/ui/MenuItem.svelte';
	import { colorVar } from '#lib/screens/sessions/colors.js';
	import { theme } from '#lib/theme.js';
	import { stopSession, resumeSession, continueSessionQueue, resolveOwnerLost, controlBusy } from './controls';
	import { RESOLVE_WARNING, runLabel } from './pauseCards';
	import LimitsStrip from '#lib/screens/sessions/LimitsStrip.svelte';
	import type { ProviderLimits } from '#lib/stores/limits.js';
	import { limitThresholds } from '#lib/stores/limitThresholds.svelte.js';
	import { runningCount } from '#lib/stores/delegation.js';
	import { liveNodes } from './delegationNodes';
	import { isSubagentCall } from '#lib/timeline/rows.js';
	import { ACCEPT_LABEL, acceptView, type AcceptDetail } from './accept';

	interface DetailLike {
		title?: string;
		color?: string | null;
		harness?: { title?: string; provider?: string };
		machine?: { name?: string };
		model?: string;
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
		providers = [],
		now = Date.now(),
		machine = null,
		onOpenSheet,
		onOpenSettings,
		onOpenDiagnostics,
		onOpenDelegation,
		onOpenMessages,
		onOpenAlarms,
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
		onOpenAlarms?: () => void;
		onAccept?: () => void;
		/** Название сессии, запустившей эту; пусто, пока список её не знает. */
		parentTitle?: string | null;
		/** Живых делегированных сессий: идут в счётчик делегирования. */
		childRunning?: number;
		/** У сессии есть делегированные: вход в делегирование виден и без субагентов. */
		hasChildren?: boolean;
	} = $props();

	let menuOpen = $state(false);
	const delegated = $derived(hasChildren || chat.timeline.entries.some((entry) => entry.kind === 'block' && isSubagentCall(entry.block)));
	const alarmCount = $derived(
		chat.alarms.filter((alarm) => alarm.state === 'armed' || alarm.state === 'failed').length
	);
	const running = $derived(delegated ? runningCount(liveNodes(chat)) + childRunning : 0);
	let confirmingForce = $state(false);
	let actionError = $state<string | null>(null);

	const detail = $derived(chat.detail as DetailLike | null);
	const parentId = $derived(((chat.detail as AcceptDetail | null)?.parent_id) ?? null);
	const accept = $derived(acceptView(chat.detail as AcceptDetail | null));
	const settings = $derived(chat.settings as SettingsLike | null);

	const title = $derived(detail?.title || 'Сессия');
	const color = $derived(colorVar(detail?.color ?? null));
	const stateLine = $derived(chatStateLine(chat.activity as Activity | null, chat.phase, (chat.execution as { state?: string })?.state, now));
	const TONE_MARK = { accent: '--accent', warning: '--warning', danger: '--danger' } as const;
	const markColor = $derived(stateLine.tone === 'default' ? color : TONE_MARK[stateLine.tone]);
	const harnessTitle = $derived(detail?.harness?.title || 'Claude Code');
	const machineName = $derived(detail?.machine?.name || 'workstation');
	const provider = $derived(detail?.harness?.provider || 'claude');

	const effectiveModel = $derived(modelLabel(settings?.effective?.model || detail?.model || 'opus-5.5'));
	const pendingModel = $derived(settings?.pending?.model && modelLabel(settings.pending.model));

	const subtitle = $derived(
		pendingModel
			? `${harnessTitle} · ${effectiveModel} → ${pendingModel} со следующего хода · ${machineName}`
			: `${harnessTitle} · ${effectiveModel} · ${machineName}`
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
	const stopState = $derived(stopping ? { 'aria-disabled': true, 'aria-busy': true } : {});
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


<div class="chat-header" role="region" aria-label="Шапка чата">
	<div class="topbar" class:tall={!!parentId}>
		{#if !pane.desktop}
			<a
				class="icon-button back"
				href={resolve('sessions')}
				aria-label="Все сессии"
			><Icon name="back" /></a>
		{/if}

		<div class="header-info">
			<div class="title-row">
				{#if detail}
					<span class="mark" style:background="var({markColor})"></span>
					<h1 class="header-title" {title}>{title}</h1>
				{:else}
					<span class="skeleton" style:width="var(--bar-56)"></span>
				{/if}
			</div>
			<div class="sub-row">
				{#if detail}
					<StateChip
						activity={chat.activity as Activity | null}
						phase={chat.phase}
						executionState={(chat.execution as { state?: string })?.state}
						now={now}
					/>
					<div class="subtitle" title={subtitle}>{subtitle}</div>
				{:else}
					<span class="skeleton" style:width="var(--bar-74)"></span>
				{/if}
			</div>
			{#if parentId}
				<div class="parent-row">
					<a class="parent-link" data-parent-link href={resolve(`c/${parentId}`)}>Запущена сессией {parentTitle || 'без названия'}</a>
				</div>
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
				<button type="button" class="pane-action" aria-pressed={pane.view === 'delegation'} onclick={() => pane.toggle('delegation')}>{running > 0 ? `Делегирование · ${running}` : 'Делегирование'}</button>
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
			{#if isLive}
				<button
					type="button"
					class="icon-button stop-btn"
					aria-label="Остановить сессию"
					title="Остановить сессию"
					{...stopState}
					onclick={() => { if (!stopping) void handleStop(false); }}
				>
					<Icon name="stop" />
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

	{#if stopping}
		<p class="header-status" role="status">Останавливаю сессию</p>
	{:else if actionError && !menuOpen}
		<p class="header-status header-error" role="alert">{actionError}</p>
	{:else if machineDown}
		<p class="header-status machine-down" data-machine-down>{machineDown}</p>
	{/if}

	{#if providers.length > 0}
		<div class="header-limits">
			<LimitsStrip
				providers={providers}
				now={now}
				thresholds={limitThresholds}
				prefer={provider}
				compact
			/>
		</div>
	{/if}

	{#if menuOpen}
		<div class="session-dropdown" use:dismissable={{ onclose: closeMenu, keep: (target) => menuButton?.contains(target) ?? false }}>
			<Menu label="Действия над сессией">
				{#if actionError}
					<div class="action-error" role="alert">{actionError}</div>
				{/if}

				{#if onOpenSheet}
					<MenuItem icon="pin" onclick={() => { menuOpen = false; onOpenSheet(); }}>Название, цвет и группы</MenuItem>
				{/if}

				{#if onOpenSettings}
					<MenuItem icon="sliders" onclick={() => { menuOpen = false; onOpenSettings(); }}>Настройки сессии</MenuItem>
				{/if}

				{#if onOpenDiagnostics}
					<MenuItem icon="info" onclick={() => { menuOpen = false; onOpenDiagnostics(); }}>Диагностика</MenuItem>
				{/if}

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

				<div class="menu-divider" role="separator"></div>

				{#if accept.available && onAccept}
					<MenuItem icon="trash" onclick={() => { menuOpen = false; onAccept(); }}>{ACCEPT_LABEL}</MenuItem>
				{/if}

				{#if isLive}
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
	.header-info {
		flex: 1;
		display: flex;
		flex-direction: column;
		gap: var(--s1);
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
		width: var(--mark-large);
		height: var(--mark-large);
		border-radius: var(--radius-round);
		flex-shrink: 0;
	}
	/* Длинное состояние уводит подзаголовок на свою строку, а не сжимает его до буквы. */
	.sub-row {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: var(--s1) var(--s3);
		min-width: 0;
		padding-left: calc(var(--mark-large) + var(--s3));
	}
	.subtitle {
		flex: 1 0 calc(var(--s4) * 8);
		min-width: 0;
		font-size: var(--fs-sm);
		color: var(--muted);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.parent-row {
		min-width: 0;
		padding-left: calc(var(--mark-large) + var(--s3));
	}
	/* Зона нажатия 44px держится отступами, а не высотой строки, чтобы ссылка не раздвигала шапку. */
	.parent-link {
		--pad: calc((var(--target) - var(--fs-sm) * var(--lh-tight)) / 2);
		display: block;
		padding-block: var(--pad);
		margin-block: calc(-1 * var(--pad));
		line-height: var(--lh-tight);
		font-size: var(--fs-sm);
		color: var(--accent-link);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.parent-link:focus-visible {
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
	.icon-button[aria-disabled='true'] {
		color: var(--muted);
		cursor: default;
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
</style>
