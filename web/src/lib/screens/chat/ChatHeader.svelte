<script lang="ts">
	import { resolve } from '$app/paths';
	import type { ChatStore } from '$lib/stores/chat.svelte';
	import type { Activity } from '$lib/screens/sessions/activityText';
	import StateChip from './StateChip.svelte';
	import { colorVar } from '$lib/screens/sessions/colors';
	import { theme } from '$lib/theme';
	import { stopSession, resumeSession, continueSessionQueue, startSessionExecution } from './controls';
	import LimitsStrip from '$lib/screens/sessions/LimitsStrip.svelte';
	import type { LimitThresholds, ProviderLimits } from '$lib/stores/limits';

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

	const isLive = $derived(
		chat.execution
			? ((chat.execution as { live?: boolean; state?: string }).live ??
			  ['created', 'starting', 'running', 'stopping'].includes(
					(chat.execution as { state?: string }).state ?? ''
			  ))
			: false
	);
	const hasUserStop = $derived(
		chat.queue.pause_reasons.some((p) => (p as { reason?: string })?.reason === 'user_stop')
	);
	const hasNoLiveExecution = $derived(
		chat.queue.hold_reasons.some((h) => (h as { reason?: string })?.reason === 'no_live_execution')
	);

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

<div class="chat-header" role="region" aria-label="Шапка чата">
	<div class="topbar">
		<a class="icon-button back" href={resolve('/sessions')} aria-label="Все сессии">
			<svg viewBox="0 0 24 24" aria-hidden="true"><path d="m13 5-7 7 7 7" /></svg>
		</a>

		<div class="grow header-info">
			<div class="row title-row">
				<span class="mark" style:background="var({color})"></span>
				<h1 class="header-title">{title}</h1>
				<StateChip
					activity={chat.activity as Activity | null}
					phase={chat.phase}
					executionState={(chat.execution as { state?: string })?.state}
					{now}
				/>
			</div>
			<div class="subtitle" title={subtitle}>{subtitle}</div>
		</div>

		<div class="header-actions">
			<button
				type="button"
				class="icon-button menu-btn"
				aria-label="Меню сессии"
				aria-haspopup="true"
				aria-expanded={menuOpen}
				onclick={() => { menuOpen = !menuOpen; confirmingForce = false; }}
			>
				<svg viewBox="0 0 24 24" aria-hidden="true">
					<circle cx="5" cy="12" r="1.5" />
					<circle cx="12" cy="12" r="1.5" />
					<circle cx="19" cy="12" r="1.5" />
				</svg>
			</button>

			<button
				type="button"
				class="icon-button theme-toggle-btn"
				data-theme-toggle
				aria-label="Сменить тему"
				onclick={() => theme.toggle()}
			>
				<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M20 14A9 9 0 0 1 10 3a9 9 0 1 0 10 11Z" /></svg>
			</button>
		</div>
	</div>

	{#if providers.length > 0}
		<div class="header-limits">
			<LimitsStrip {providers} {now} {thresholds} prefer={provider} crunch={chat.crunch.enabled} />
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
		border-bottom: var(--hairline) solid var(--line);
		background: var(--surface);
		z-index: 5;
	}
	.topbar {
		display: flex;
		align-items: center;
		gap: var(--s2);
		padding: var(--s2) var(--s4);
		min-height: var(--target);
	}
	.header-info {
		min-width: 0;
	}
	.title-row {
		display: flex;
		align-items: center;
		gap: var(--s2);
		min-width: 0;
	}
	.header-title {
		margin: 0;
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
	.subtitle {
		font-size: var(--fs-meta);
		color: var(--muted);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.header-actions {
		display: flex;
		align-items: center;
		gap: var(--s1);
		margin-left: auto;
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
		border-radius: var(--radius-card);
	}
	.icon-button svg {
		width: var(--icon);
		height: var(--icon);
		fill: none;
		stroke: currentColor;
		stroke-width: 2;
	}
	.grow {
		flex: 1;
	}
	.header-limits {
		padding: 0 var(--s4) var(--s2);
	}
	.session-dropdown {
		position: absolute;
		right: var(--s4);
		top: calc(100% + var(--s1));
		background: var(--surface);
		border: var(--hairline) solid var(--line);
		border-radius: var(--radius-card);
		box-shadow: var(--shadow);
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
