<script lang="ts" module>
	export function formatRejectedSettings(rejected: { keys: string[]; reason: string }): string {
		return `Настройка недоступна и сброшена: ${rejected.keys.join(', ')}`;
	}
</script>

<script lang="ts">
	import type { ChatStore } from '$lib/stores/chat.svelte';
	import type { MachineRecord } from '$lib/stores/machines.svelte';
	import { updateSettings, updateCrunch } from './controls';
	import { ApiError } from '$lib/api/http';
	import Toggle from '$lib/ui/Toggle.svelte';

	let {
		chat,
		machine = null,
		desktop = false,
		onclose
	}: {
		chat: ChatStore;
		machine?: MachineRecord | null;
		desktop?: boolean;
		onclose?: () => void;
	} = $props();

	// Harness and catalog lookup
	const harnessId = $derived((chat.detail?.harness as { id?: string })?.id ?? 'claude');
	const harness = $derived(machine?.harnesses?.find((h) => h.id === harnessId));

	// Effective settings
	const effective = $derived(
		(chat.settings?.effective as { model?: string; effort?: string; permission_mode?: string }) ?? {
			model: (chat.detail?.model as string) ?? 'opus',
			effort: (chat.detail?.effort as string) ?? 'medium',
			permission_mode: (chat.detail?.permission_mode as string) ?? 'default'
		}
	);

	// Pending settings
	const pending = $derived(
		(chat.settings?.pending as { model?: string; effort?: string; permission_mode?: string } | null) ?? null
	);

	// Whether midsession settings are supported by harness execution
	const midsessionSupported = $derived(
		chat.execution ? (chat.execution.settings_midsession !== false) : true
	);

	// Local form state
	let selectedModel = $state('opus');
	let selectedEffort = $state('medium');
	let selectedPermission = $state('default');
	let crunchEnabled = $state(false);
	let resumeText = $state('Лимиты восстановлены, продолжай с того места, где остановился.');

	// Error state per field
	let modelError = $state<string | null>(null);
	let effortError = $state<string | null>(null);
	let permissionError = $state<string | null>(null);
	let generalError = $state<string | null>(null);
	let saving = $state(false);

	// Sync local values when effective/pending changes
	$effect(() => {
		selectedModel = pending?.model ?? effective.model ?? 'opus';
		selectedEffort = pending?.effort ?? effective.effort ?? 'medium';
		selectedPermission = pending?.permission_mode ?? effective.permission_mode ?? 'default';
		crunchEnabled = chat.crunch.enabled;
		if (chat.crunch.resume_text) resumeText = chat.crunch.resume_text;
	});

	// Available models from harness
	const availableModels = $derived(
		harness?.models ?? [{ id: selectedModel, title: selectedModel, efforts: ['low', 'medium', 'high'] }]
	);

	// Available efforts for selected model
	const currentModelRecord = $derived(availableModels.find((m) => m.id === selectedModel));
	const availableEfforts = $derived(currentModelRecord?.efforts ?? ['low', 'medium', 'high']);

	// Available permission modes
	const availablePermissionModes = $derived(
		harness?.permission_modes ?? ['default', 'ask', 'auto_edit', 'full_access']
	);

	const EFFORT_LABELS: Record<string, string> = {
		low: 'Низкое',
		medium: 'Обычное',
		high: 'Высокое',
		xhigh: 'Максимальное'
	};

	const PERMISSION_LABELS: Record<string, string> = {
		default: 'По умолчанию',
		ask: 'Спрашивать',
		auto_edit: 'Автоправки',
		full_access: 'Полный доступ'
	};

	async function saveSettings(event?: SubmitEvent) {
		if (event) event.preventDefault();
		if (saving) return;
		saving = true;
		modelError = null;
		effortError = null;
		permissionError = null;
		generalError = null;

		try {
			await updateSettings(chat.id, {
				model: selectedModel,
				effort: selectedEffort,
				permission_mode: selectedPermission
			});

			await updateCrunch(chat.id, {
				enabled: crunchEnabled,
				resume_text: resumeText
			});

			if (onclose && !desktop) onclose();
		} catch (err) {
			if (err instanceof ApiError && err.status === 422) {
				const code = err.code;
				if (code === 'model_unknown') {
					modelError = err.message || 'Модель неизвестна или недоступна на этой машине';
				} else if (code === 'effort_not_allowed') {
					effortError = err.message || 'Усилие не поддерживается для этой модели';
				} else if (code === 'permission_mode_unknown') {
					permissionError = err.message || 'Режим разрешений не поддерживается';
				} else if (code === 'settings_midsession_unsupported') {
					generalError = 'Настройка посреди сессии не поддерживается';
				} else {
					generalError = err.message || 'Ошибка валидации настроек';
				}
			} else if (err instanceof Error) {
				generalError = err.message;
			}
		} finally {
			saving = false;
		}
	}
</script>

<div class="session-settings-container" class:desktop>
	<header class="settings-header">
		<div>
			<h2 id="settings-title">Настройки сессии</h2>
			{#if desktop && chat.detail?.title}
				<div class="subtitle">{(chat.detail as { title?: string }).title}</div>
			{/if}
		</div>
		{#if onclose}
			<button type="button" class="icon-button" aria-label="Закрыть" onclick={onclose}>
				<svg viewBox="0 0 24 24" aria-hidden="true"><path d="m6 6 12 12M6 18 18 6" /></svg>
			</button>
		{/if}
	</header>

	<form id="session-settings-form" class="settings-body scroll" onsubmit={saveSettings}>
		{#if chat.settings?.rejected}
			<div class="rejected-banner" role="alert">
				{formatRejectedSettings(chat.settings.rejected)}
			</div>
		{/if}

		{#if generalError}
			<div class="error-banner" role="alert">
				{generalError}
			</div>
		{/if}

		<div class="v3-well">
			<div class="row">
				<span class="live-dot" class:active={chat.execution?.live}></span>
				<strong>{chat.execution?.live ? 'Сессия работает' : 'Сессия в ожидании'}</strong>
			</div>
			<p class="v3-note">
				{(chat.detail?.harness as { title?: string })?.title ?? 'Claude Code'} ·
				{(chat.detail?.machine as { name?: string })?.name ?? 'workstation'}
				{#if chat.detail?.workdir}
					<br /><code>{chat.detail.workdir}</code>
				{/if}
			</p>
		</div>

		<div class="field-row">
			<div class="field">
				<label class="field-label" for="model">Модель</label>
				<select
					id="model"
					name="model"
					bind:value={selectedModel}
					disabled={!midsessionSupported || saving}
					aria-describedby={pending?.model ? 'model-pending' : modelError ? 'model-error' : undefined}
				>
					{#each availableModels as m (m.id)}
						<option value={m.id}>{m.title || m.id}</option>
					{/each}
				</select>
				{#if pending?.model}
					<p id="model-pending" class="hint">
						{effective.model} → {pending.model} со следующего хода
					</p>
				{/if}
				{#if !midsessionSupported}
					<p class="field-error">Настройка посреди сессии не поддерживается</p>
				{:else if modelError}
					<p id="model-error" class="field-error">{modelError}</p>
				{/if}
			</div>

			<div class="field">
				<label class="field-label" for="effort">Усилие</label>
				<select
					id="effort"
					name="effort"
					bind:value={selectedEffort}
					disabled={!midsessionSupported || saving}
					aria-describedby={pending?.effort ? 'effort-pending' : effortError ? 'effort-error' : undefined}
				>
					{#each availableEfforts as eff (eff)}
						<option value={eff}>{EFFORT_LABELS[eff] || eff}</option>
					{/each}
				</select>
				{#if pending?.effort}
					<p id="effort-pending" class="hint">
						Применится со следующего хода
					</p>
				{/if}
				{#if effortError}
					<p id="effort-error" class="field-error">{effortError}</p>
				{/if}
			</div>
		</div>

		<div class="v3-mode">
			<h3>Режим разрешений</h3>
			<p class="v3-note">
				Сейчас: <strong class="secondary">{effective.permission_mode ? (PERMISSION_LABELS[effective.permission_mode] || effective.permission_mode) : 'По умолчанию'}</strong>
			</p>
			<div class="field">
				<label class="field-label" for="permission">Новый режим</label>
				<select
					id="permission"
					name="permission"
					bind:value={selectedPermission}
					disabled={!midsessionSupported || saving}
					aria-describedby={pending?.permission_mode ? 'permission-pending' : permissionError ? 'permission-error' : undefined}
				>
					{#each availablePermissionModes as mode (mode)}
						<option value={mode}>{PERMISSION_LABELS[mode] || mode}</option>
					{/each}
				</select>
				{#if pending?.permission_mode}
					<p id="permission-pending" class="hint">
						Применится со следующего хода
					</p>
				{/if}
				{#if permissionError}
					<p id="permission-error" class="field-error">{permissionError}</p>
				{/if}
			</div>
		</div>

		<p class="v3-caption">Текущий ход продолжится с прежними настройками.</p>

		<section class="crunch">
			<div class="row between">
				<div class="grow">
					<h3>Кранч</h3>
					<p class="v3-note">Продолжать после восстановления лимитов</p>
				</div>
				<Toggle label="Кранч включён" bind:checked={crunchEnabled} />
			</div>
			<div class="field">
				<label class="field-label" for="resume">Текст продолжения</label>
				<textarea
					id="resume"
					name="resume"
					rows="3"
					bind:value={resumeText}
					disabled={saving}
				></textarea>
				<p class="hint">Отправится один раз после паузы по лимиту.</p>
			</div>
		</section>

		<footer class="footer-action">
			<button class="button primary full" type="submit">
				{saving ? 'Сохранение…' : 'Сохранить настройки'}
			</button>
			<p class="v3-note">Модель и усилие тоже изменятся со следующего хода.</p>
		</footer>
	</form>
</div>

<style>
	.session-settings-container {
		display: flex;
		flex-direction: column;
		height: 100%;
		background: var(--surface);
		color: var(--text);
		padding: var(--s4);
		box-sizing: border-box;
	}
	.settings-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		margin-bottom: var(--s4);
	}
	.settings-header h2 {
		margin: 0;
		font-size: var(--fs-title);
		font-weight: var(--weight-semibold);
	}
	.subtitle {
		font-size: var(--fs-meta);
		color: var(--muted);
	}
	.settings-body {
		flex: 1;
		display: flex;
		flex-direction: column;
		gap: var(--s4);
		overflow-y: auto;
	}
	.v3-well {
		padding: var(--s3);
		border-radius: var(--radius-card);
		background: var(--raised);
		border: var(--hairline) solid var(--line);
	}
	.row {
		display: flex;
		align-items: center;
		gap: var(--s2);
	}
	.row.between {
		justify-content: space-between;
	}
	.grow {
		flex: 1;
	}
	.live-dot {
		width: var(--mark);
		height: var(--mark);
		border-radius: var(--radius-round);
		background: var(--muted);
	}
	.live-dot.active {
		background: var(--accent);
	}
	.v3-note {
		margin: var(--s1) 0 0;
		font-size: var(--fs-meta);
		color: var(--muted);
	}
	.field-row {
		display: flex;
		gap: var(--s3);
	}
	.field {
		flex: 1;
		display: flex;
		flex-direction: column;
		gap: var(--s1);
	}
	.field-label {
		font-size: var(--fs-meta);
		color: var(--muted);
	}
	select, textarea {
		padding: var(--s2);
		border-radius: var(--radius-card);
		border: var(--hairline) solid var(--line);
		background: var(--surface);
		color: var(--text);
		font-size: var(--fs-body);
	}
	select:disabled, textarea:disabled {
		opacity: 0.5;
		cursor: not-allowed;
	}
	.hint {
		margin: 0;
		font-size: var(--fs-meta);
		color: var(--accent);
	}
	.field-error {
		margin: 0;
		font-size: var(--fs-meta);
		color: var(--danger);
	}
	.rejected-banner, .error-banner {
		padding: var(--s2) var(--s3);
		border-radius: var(--radius-card);
		font-size: var(--fs-meta);
		background: var(--danger-soft);
		border: var(--hairline) solid var(--danger);
		color: var(--danger);
	}
	.v3-mode h3, .crunch h3 {
		margin: 0 0 var(--s1);
		font-size: var(--fs-body);
		font-weight: var(--weight-semibold);
	}
	.v3-caption {
		margin: 0;
		font-size: var(--fs-meta);
		color: var(--muted);
	}
	.crunch {
		padding: var(--s3);
		border-radius: var(--radius-card);
		border: var(--hairline) solid var(--line);
		display: flex;
		flex-direction: column;
		gap: var(--s3);
	}
	.footer-action {
		margin-top: auto;
		display: flex;
		flex-direction: column;
		gap: var(--s2);
		padding-top: var(--s4);
	}
	.button.primary {
		padding: var(--s3);
		border-radius: var(--radius-card);
		background: var(--accent);
		color: var(--accent-ink);
		border: none;
		font-weight: var(--weight-semibold);
		cursor: pointer;
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
