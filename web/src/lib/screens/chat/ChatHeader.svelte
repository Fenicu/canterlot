<script lang="ts">
	import { errorText } from '#lib/api/errors.js';
	import { resolve } from '$app/paths';
	import type { ChatStore } from '#lib/stores/chat.svelte.js';
	import type { Activity } from '#lib/screens/sessions/activityText.js';
	import StateChip from './StateChip.svelte';
	import { chatStateLine } from './stateLine';
	import { modelLabel } from '#lib/format/labels.js';
	import Icon from '#lib/ui/Icon.svelte';
	import { dismissable, type DismissReason } from '#lib/ui/dismiss.js';
	import { pane } from '#lib/shell/pane.svelte.js';
	import Menu from '#lib/ui/Menu.svelte';
	import MenuItem from '#lib/ui/MenuItem.svelte';
	import { colorVar } from '#lib/screens/sessions/colors.js';
	import { theme } from '#lib/theme.js';
	import { stopSession, resumeSession, continueSessionQueue } from './controls';
	import { runLabel } from './pauseCards';
	import LimitsStrip from '#lib/screens/sessions/LimitsStrip.svelte';
	import type { LimitThresholds, ProviderLimits } from '#lib/stores/limits.js';

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

	let {
		chat,
		providers = [],
		thresholds = { warn_pcts: [80, 90], weekly_reserve_pct: 95, stale_after_secs: 1800 },
		now = Date.now(),
		onOpenSheet,
		onOpenSettings,
		onOpenDiagnostics
	}: {
		chat: ChatStore;
		providers?: ProviderLimits[];
		thresholds?: LimitThresholds;
		now?: number;
		onOpenSheet?: () => void;
		onOpenSettings?: () => void;
		onOpenDiagnostics?: () => void;
	} = $props();

	let menuOpen = $state(false);
	const delegated = $derived(chat.timeline.entries.some((entry) => entry.kind === 'block' && entry.block.toolName === 'Agent'));
	let confirmingForce = $state(false);
	let actionError = $state<string | null>(null);

	const detail = $derived(chat.detail as DetailLike | null);
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

	const isLive = $derived(chat.execution
		? (chat.execution as { live?: boolean; state?: string }).live ?? ['created', 'starting', 'running', 'stopping'].includes((chat.execution as { state?: string }).state ?? '')
		: false);

	const hasUserStop = $derived(chat.queue.pause_reasons.some((p) => (p as { reason?: string })?.reason === 'user_stop'));
	const ownerLost = $derived((chat.execution as { state?: string } | null)?.state === 'owner_lost');

	async function handleStop(force = false) {
		actionError = null;
		try {
			await stopSession(chat.id, { force });
			menuOpen = false;
			confirmingForce = false;
		} catch (err: unknown) {
			actionError = errorText(err, 'Не удалось остановить сессию');
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
	<div class="topbar">
		<a
			class="icon-button back"
			href={resolve('sessions')}
			aria-label="Все сессии"
		><Icon name="back" /></a>

		<div class="header-info">
			<div class="title-row">
				{#if detail}
					<span class="mark" style:background="var({markColor})"></span>
					<h1 class="header-title">{title}</h1>
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
		</div>

		<div class="header-actions">
			{#if pane.desktop && delegated}
				<button type="button" class="pane-action" aria-pressed={pane.view === 'delegation'} onclick={() => pane.toggle('delegation')}>Делегирование</button>
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

			<button
				type="button"
				class="icon-button theme-toggle-btn"
				data-theme-toggle
				aria-label="Сменить тему"
				onclick={() => theme.toggle()}
			>
				<Icon name={theme.effective === 'dark' ? 'sun' : 'moon'} />
			</button>
		</div>
	</div>

	{#if providers.length > 0}
		<div class="header-limits">
			<LimitsStrip
				providers={providers}
				now={now}
				thresholds={thresholds}
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

				<div class="menu-divider" role="separator"></div>

				{#if isLive}
					<MenuItem icon="stop" onclick={() => void handleStop(false)}>Остановить</MenuItem>

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
				{:else if !ownerLost}
					<MenuItem onclick={() => void handleRun()}>{runLabel(chat.turnRecorded)}</MenuItem>
				{/if}

				{#if isLive && hasUserStop}
					<MenuItem onclick={() => void handleContinueQueue()}>Продолжить очередь</MenuItem>
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
	.sub-row {
		display: flex;
		align-items: center;
		gap: var(--s3);
		min-width: 0;
	}
	.subtitle {
		flex: 1 1 0;
		min-width: 0;
		font-size: var(--fs-sm);
		color: var(--muted);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
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
		height: var(--hairline);
		background: var(--control-line);
		margin: var(--s2) var(--s4);
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
