<script lang="ts">
	import { resolve } from '$app/paths';
	import type { ChatStore } from '#lib/stores/chat.svelte.js';
	import type { Activity } from '#lib/screens/sessions/activityText.js';
	import StateChip from './StateChip.svelte';
	import { chatStateLine } from './stateLine';
	import Icon from '#lib/ui/Icon.svelte';
	import { colorVar } from '#lib/screens/sessions/colors.js';
	import { theme } from '#lib/theme.js';
	import { stopSession, resumeSession, continueSessionQueue, startSessionExecution } from './controls';
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
		onOpenSettings,
		onOpenDiagnostics
	}: {
		chat: ChatStore;
		providers?: ProviderLimits[];
		thresholds?: LimitThresholds;
		now?: number;
		onOpenSettings?: () => void;
		onOpenDiagnostics?: () => void;
	} = $props();

	let menuOpen = $state(false);
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

	const effectiveModel = $derived(settings?.effective?.model || detail?.model || 'opus-5.5');
	const pendingModel = $derived(settings?.pending?.model);

	const subtitle = $derived(
		pendingModel
			? `${harnessTitle} · ${effectiveModel} → ${pendingModel} со следующего хода · ${machineName}`
			: `${harnessTitle} · ${effectiveModel} · ${machineName}`
	);

	const isLive = $derived(chat.execution
		? (chat.execution as { live?: boolean; state?: string }).live ?? ['created', 'starting', 'running', 'stopping'].includes((chat.execution as { state?: string }).state ?? '')
		: false);

	const hasUserStop = $derived(chat.queue.pause_reasons.some((p) => (p as { reason?: string })?.reason === 'user_stop'));
	const hasNoLiveExecution = $derived(chat.queue.hold_reasons.some((h) => (h as { reason?: string })?.reason === 'no_live_execution'));

	async function handleStop(force = false) {
		actionError = null;
		try {
			await stopSession(chat.id, { force });
			menuOpen = false;
			confirmingForce = false;
		} catch (err: unknown) {
			actionError = err instanceof Error ? err.message : 'Не удалось остановить сессию';
		}
	}

	async function handleResume() {
		actionError = null;
		try {
			await resumeSession(chat.id);
			menuOpen = false;
		} catch (err: unknown) {
			actionError = err instanceof Error ? err.message : 'Не удалось возобновить сессию';
		}
	}

	async function handleContinueQueue() {
		actionError = null;
		try {
			await continueSessionQueue(chat.id);
			menuOpen = false;
		} catch (err: unknown) {
			actionError = err instanceof Error ? err.message : 'Не удалось продолжить очередь';
		}
	}

	let menuButton = $state<HTMLButtonElement | null>(null);

	function closeMenuOnEscape(event: KeyboardEvent) {
		if (event.key !== 'Escape' || !menuOpen) return;
		menuOpen = false;
		confirmingForce = false;
		menuButton?.focus();
	}

	async function handleStart() {
		actionError = null;
		try {
			await startSessionExecution(chat.id);
			menuOpen = false;
		} catch (err: unknown) {
			actionError = err instanceof Error ? err.message : 'Не удалось запустить сессию';
		}
	}
</script>

<svelte:window onkeydown={closeMenuOnEscape} />

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
				crunch={chat.crunch.enabled}
			/>
		</div>
	{/if}

	{#if menuOpen}
		<div class="session-dropdown" role="menu" aria-label="Действия над сессией">
			{#if actionError}
				<div class="action-error" role="alert">{actionError}</div>
			{/if}

			{#if onOpenSettings}
				<button
					type="button"
					role="menuitem"
					class="menu-item"
					onclick={() => { menuOpen = false; onOpenSettings(); }}
				>
					Настройки сессии
				</button>
			{/if}

			{#if onOpenDiagnostics}
				<button
					type="button"
					role="menuitem"
					class="menu-item"
					onclick={() => { menuOpen = false; onOpenDiagnostics(); }}
				>
					Диагностика
				</button>
			{/if}

			<div class="menu-divider" role="separator"></div>

			{#if isLive}
				<button
					type="button"
					role="menuitem"
					class="menu-item"
					onclick={() => void handleStop(false)}
				>
					Остановить
				</button>

				{#if !confirmingForce}
					<button
						type="button"
						role="menuitem"
						class="menu-item danger"
						onclick={() => { confirmingForce = true; }}
					>
						Принудительно остановить
					</button>
				{:else}
					<div class="confirm-box" role="dialog" aria-label="Подтверждение остановки">
						<p>Принудительно прервать выполнение?</p>
						<div class="row">
							<button type="button" class="btn-confirm-danger" onclick={() => void handleStop(true)}>
								Остановить принудительно
							</button>
							<button type="button" class="btn-cancel" onclick={() => { confirmingForce = false; }}>
								Отмена
							</button>
						</div>
					</div>
				{/if}
			{:else}
				<button
					type="button"
					role="menuitem"
					class="menu-item"
					onclick={() => void handleResume()}
				>
					Возобновить сессию
				</button>
				{#if hasNoLiveExecution && chat.queue.queued === 0}
					<button
						type="button"
						role="menuitem"
						class="menu-item"
						onclick={() => void handleStart()}
					>
						Запустить
					</button>
				{/if}
			{/if}

			{#if isLive && hasUserStop}
				<button
					type="button"
					role="menuitem"
					class="menu-item"
					onclick={() => void handleContinueQueue()}
				>
					Продолжить очередь
				</button>
			{/if}
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
		padding: var(--s2) var(--s3);
		margin: var(--s5) var(--gutter) var(--s3);
		border-radius: var(--radius-pill);
		background: var(--surface);
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
	.header-limits {
		padding: 0 var(--gutter) var(--s2);
	}
	.session-dropdown {
		position: absolute;
		right: var(--s4);
		top: calc(100% + var(--s1));
		background: var(--popover);
		border-radius: var(--radius-card);
		display: flex;
		flex-direction: column;
		min-width: var(--preview-height);
		padding: var(--s2);
		z-index: 20;
	}
	.menu-item {
		background: var(--transparent);
		border: none;
		text-align: left;
		padding: var(--s2) var(--s3);
		border-radius: var(--radius-card);
		color: var(--text);
		font-size: var(--fs-body);
		cursor: pointer;
	}
	.menu-item:hover {
		background: var(--raised);
	}
	.menu-item.danger {
		color: var(--danger);
	}
	.menu-divider {
		height: var(--hairline);
		background: var(--line);
		margin: var(--s1) 0;
	}
	.confirm-box {
		padding: var(--s2);
		background: var(--raised);
		border-radius: var(--radius-card);
		font-size: var(--fs-meta);
	}
	.confirm-box p {
		margin: 0 0 var(--s2);
	}
	.btn-confirm-danger {
		background: var(--danger);
		color: var(--text);
		border: none;
		padding: var(--s1) var(--s2);
		border-radius: var(--radius-card);
		font-size: var(--fs-meta);
		cursor: pointer;
	}
	.btn-cancel {
		background: var(--transparent);
		color: var(--muted);
		border: none;
		padding: var(--s1) var(--s2);
		font-size: var(--fs-meta);
		cursor: pointer;
	}
	.action-error {
		padding: var(--s1) var(--s2);
		font-size: var(--fs-meta);
		color: var(--danger);
		margin-bottom: var(--s1);
	}
</style>
