<script lang="ts" module>
	import { settingKeyLabel } from '#lib/format/labels.js';

	export function formatRejectedSettings(rejected: { keys: string[]; reason: string }): string {
		return `Настройка недоступна и сброшена: ${rejected.keys.map(settingKeyLabel).join(', ')}`;
	}
</script>

<script lang="ts">
	import { errorText } from '#lib/api/errors.js';
	import { executionLive, type ChatStore } from '#lib/stores/chat.svelte.js';
	import type { MachineRecord } from '#lib/stores/machines.svelte.js';
	import { updateSettings, updateCrunch, type SettingsPatch } from './controls';
	import { listPresets } from '#lib/api/client.js';
	import { untrack } from 'svelte';
	import type { components } from '#lib/api/generated/openapi.js';
	import { ApiError } from '#lib/api/http.js';
	import { shouldShowApplyNewVersion } from '../presets/presetsModel.js';
	import { soleEffort } from '../sessions/newSessionModel.js';
	import Button from '#lib/ui/Button.svelte';
	import IconButton from '#lib/ui/IconButton.svelte';
	import Select from '#lib/ui/Select.svelte';
	import Textarea from '#lib/ui/Textarea.svelte';
	import Toggle from '#lib/ui/Toggle.svelte';
	import {
		PERMISSION_MODES,
		effortLabel,
		modelLabel,
		permissionHint,
		permissionLabel,
		modelImagesNote,
		permissionValue,
		presetChangeNote
	} from '#lib/format/labels.js';

	type AppliedPreset = components['schemas']['AppliedPreset'];
	type PresetSummary = components['schemas']['PresetSummary'];

	interface PresetSettings {
		instruction_profile?: string | null;
		instruction_preset?: AppliedPreset | null;
	}

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
	const provider = $derived((chat.detail?.harness as { provider?: string })?.provider ?? 'claude');
	const harness = $derived(machine?.harnesses?.find((h) => h.id === harnessId));

	// Effective settings
	const effective = $derived(
		(chat.settings?.effective as { model?: string; effort?: string; permission_mode?: string } & PresetSettings) ?? {
			model: (chat.detail?.model as string) ?? 'opus',
			effort: (chat.detail?.effort as string) ?? 'medium',
			permission_mode: (chat.detail?.permission_mode as string) ?? ''
		}
	);

	// Pending settings
	const pending = $derived(
		(chat.settings?.pending as { model?: string; effort?: string; permission_mode?: string } | null & PresetSettings) ?? null
	);

	// Пресеты инструкций для выбора: подгружаются при открытии листа.
	let presets = $state<PresetSummary[]>([]);
	let presetsError = $state<string | null>(null);
	const BUILTIN_TITLES: Record<string, string> = { clean: 'Без инструкций', owner: 'Как у владельца' };
	const presetTitle = $derived((ref: string | null | undefined): string => {
		if (!ref) return 'Без инструкций';
		const found = presets.find((item) => item.id === ref);
		if (found) return found.title;
		if (Object.hasOwn(BUILTIN_TITLES, ref)) return BUILTIN_TITLES[ref]!;
		return ref;
	});
	const effectiveRef = $derived((effective.instruction_profile as string | undefined) ?? 'clean');
	const applied = $derived((effective.instruction_preset as AppliedPreset | undefined) ?? null);
	const showApplyNewVersion = $derived(
		applied ? shouldShowApplyNewVersion({ revision: applied.revision, current_revision: applied.current_revision }) : false
	);
	const rejectedPresetDeleted = $derived(
		chat.settings?.rejected?.reason === 'preset_deleted' && chat.settings.rejected.keys.includes('instruction_profile')
	);
	const presetPending = $derived((pending as PresetSettings | null)?.instruction_profile ?? null);
	const presetOptions = $derived(
		presets.length > 0
			? presets
			: (Object.entries(BUILTIN_TITLES) as [string, string][]).map(([id, title]) => ({
					id,
					kind: (id === 'clean' ? 'clean' : 'owner') as 'clean' | 'owner',
					title,
					description: '',
					revision: null,
					text_bytes: null,
					created: { by: 'builtin', conversation_id: null, conversation_title: null, at: null },
					last_edit: null,
					used_by: 0
				}))
	);

	const live = $derived(executionLive(chat.execution));
	const midsessionSupported = $derived(!live || harness?.settings_midsession !== false);

	// Local form state
	let selectedModel = $state('opus');
	let selectedEffort = $state('medium');
	let selectedPermission = $state('owner');
	let selectedPreset = $state('clean');
	let crunchEnabled = $state(false);
	let resumeText = $state('Лимиты восстановлены, продолжай с того места, где остановился.');

	// Error state per field
	let modelError = $state<string | null>(null);
	let effortError = $state<string | null>(null);
	let permissionError = $state<string | null>(null);
	let presetError = $state<string | null>(null);
	let generalError = $state<string | null>(null);
	let saving = $state(false);

	// Sync local values when effective/pending changes
	$effect(() => {
		selectedModel = pending?.model ?? effective.model ?? 'opus';
		selectedEffort = pending?.effort ?? effective.effort ?? 'medium';
		selectedPermission = permissionValue(pending?.permission_mode ?? effective.permission_mode);
		selectedPreset = (pending as PresetSettings | null)?.instruction_profile ?? effectiveRef;
		crunchEnabled = chat.crunch.enabled;
		if (chat.crunch.resume_text) resumeText = chat.crunch.resume_text;
	});

	async function loadPresets(): Promise<void> {
		presetsError = null;
		try {
			const response = await listPresets();
			presets = response.data;
		} catch {
			presetsError = 'Не удалось загрузить инструкции';
		}
	}

	$effect(() => {
		void chat.settings?.rev;
		void loadPresets();
	});

	// Available models from harness
	const availableModels = $derived(
		harness?.models ?? [{ id: selectedModel, title: selectedModel, efforts: ['low', 'medium', 'high'] }]
	);

	// Available efforts for selected model
	const currentModelRecord = $derived(availableModels.find((m) => m.id === selectedModel));
	const availableEfforts = $derived(currentModelRecord?.efforts ?? availableModels[0]?.efforts ?? ['low', 'medium', 'high']);

	$effect(() => {
		const efforts = availableEfforts;
		untrack(() => {
			selectedEffort = soleEffort(efforts, selectedEffort);
		});
	});

	// Available permission modes
	const availablePermissionModes = $derived(
		(harness?.permission_modes ?? [...PERMISSION_MODES]).filter(
			(mode) => !(provider === 'antigravity' && mode === 'read_only')
		)
	);

	// Модель сессии могли убрать из каталога: она остаётся видна, пока владелец не выберет другую.
	const modelOptions = $derived([
		...availableModels.map((m) => ({ value: m.id, label: m.title || m.id })),
		...(effective.model && !availableModels.some((m) => m.id === effective.model)
			? [{ value: effective.model, label: modelLabel(effective.model) }]
			: [])
	]);
	const effortOptions = $derived(availableEfforts.map((value) => ({ value, label: effortLabel(value) })));
	const permissionOptions = $derived(availablePermissionModes.map((value) => ({ value, label: permissionLabel(value) })));
	const presetSelectOptions = $derived(presetOptions.map((option) => ({ value: option.id, label: option.title })));
	const locked = $derived(!midsessionSupported || saving);

	async function saveSettings(event?: SubmitEvent) {
		if (event) event.preventDefault();
		if (saving) return;
		saving = true;
		modelError = null;
		effortError = null;
		permissionError = null;
		presetError = null;
		generalError = null;

		try {
			// Хаб отклоняет значение вне каталога: у сессии без выбранного режима разрешений он пуст, поэтому уходит только изменённое.
			const shown = {
				model: pending?.model ?? effective.model,
				effort: pending?.effort ?? effective.effort,
				permission_mode: pending?.permission_mode ?? effective.permission_mode,
				instruction_profile: (pending as PresetSettings | null)?.instruction_profile ?? effectiveRef
			};
			const patch: SettingsPatch = {};
			if (selectedModel !== shown.model) patch.model = selectedModel;
			if (selectedEffort !== shown.effort) patch.effort = selectedEffort;
			if (selectedPermission !== permissionValue(shown.permission_mode)) patch.permission_mode = selectedPermission;
			if (selectedPreset !== shown.instruction_profile) patch.instruction_profile = selectedPreset;
			if (Object.keys(patch).length > 0) await updateSettings(chat.id, patch);

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
				} else if (code === 'profile_unknown') {
					presetError = 'Инструкции не найдены';
				} else if (code === 'settings_midsession_unsupported') {
					generalError = 'Настройка посреди сессии не поддерживается';
				} else {
					generalError = err.message || 'Ошибка валидации настроек';
				}
			} else {
				generalError = errorText(err, 'Не удалось сохранить настройки');
			}
		} finally {
			saving = false;
		}
	}

	async function applyNewVersion(): Promise<void> {
		if (saving) return;
		saving = true;
		presetError = null;
		generalError = null;
		try {
			await updateSettings(chat.id, { instruction_profile: effectiveRef });
		} catch (err) {
			if (err instanceof ApiError && err.code === 'profile_unknown') {
				presetError = 'Инструкции не найдены';
			} else {
				generalError = errorText(err, 'Не удалось сохранить настройки');
			}
		} finally {
			saving = false;
		}
	}
</script>

<div class="settings" class:desktop data-sheet-flush>
	{#if desktop}
		<header class="pane-bar">
			<div class="titles">
				<h2>Настройки сессии</h2>
				{#if chat.detail?.title}<p>{(chat.detail as { title?: string }).title}</p>{/if}
			</div>
			{#if onclose}<IconButton icon="close" label="Закрыть" onclick={onclose} />{/if}
		</header>
	{/if}

	<form id="session-settings-form" class="settings-body" onsubmit={saveSettings}>
		{#if chat.settings?.rejected}
			<div class="alert" role="alert">{formatRejectedSettings(chat.settings.rejected)}</div>
		{/if}

		{#if generalError}
			<div class="alert" role="alert">{generalError}</div>
		{/if}

		<div class="card">
			<p class="state"><span class="live-dot" class:active={live}></span><strong>{live ? 'Сессия работает' : 'Сессия в ожидании'}</strong></p>
			<p class="note">
				{(chat.detail?.harness as { title?: string })?.title ?? 'Claude Code'} ·
				{(chat.detail?.machine as { name?: string })?.name ?? 'workstation'}
				{#if chat.detail?.workdir}
					<br /><code>{chat.detail.workdir}</code>
				{/if}
			</p>
		</div>

		<div class="pair">
			<Select
				id="model"
				name="model"
				label="Модель"
				options={modelOptions}
				bind:value={selectedModel}
				disabled={locked}
				hint={pending?.model ? `${modelLabel(effective.model ?? '')} → ${modelLabel(pending.model)} со следующего хода` : undefined}
				error={!midsessionSupported ? 'Настройка посреди сессии не поддерживается' : (modelError ?? undefined)}
			/>
			{#if provider !== 'antigravity'}
				<Select
					id="effort"
					name="effort"
					label="Усилие"
					options={effortOptions}
					bind:value={selectedEffort}
					disabled={locked}
					hint={pending?.effort ? 'Применится со следующего хода' : undefined}
					error={effortError ?? undefined}
				/>
			{/if}
		</div>
		{#if modelImagesNote(currentModelRecord?.images)}<p class="note">{modelImagesNote(currentModelRecord?.images)}</p>{/if}

		<div class="card">
			<h3>Режим разрешений</h3>
			<p class="note">Сейчас: <strong>{permissionLabel(effective.permission_mode)}</strong></p>
			<Select
				id="permission"
				name="permission"
				label="Новый режим"
				options={permissionOptions}
				bind:value={selectedPermission}
				disabled={locked}
				hint={[permissionHint(selectedPermission, provider), pending?.permission_mode ? 'Применится со следующего хода' : ''].filter(Boolean).join(' ')}
				error={permissionError ?? undefined}
			/>
		</div>

		<div class="card">
			<h3>Инструкции</h3>
			<p class="note">
				{#if applied?.deleted}
					{applied.title} (удалены): сессия работает со своей копией
				{:else}
					Сейчас: <strong>{applied?.title ?? presetTitle(effectiveRef)}{applied?.revision != null ? ` · версия ${applied.revision}` : ''}</strong>
				{/if}
			</p>
			{#if showApplyNewVersion}
				<p class="note">Инструкции изменены после применения</p>
				<Button onclick={() => void applyNewVersion()}>Применить новую версию</Button>
			{/if}
			<Select
				id="preset"
				name="preset"
				label="Новые инструкции"
				options={presetSelectOptions}
				bind:value={selectedPreset}
				disabled={locked}
				hint={presetPending ? `${presetTitle(effectiveRef)} → ${presetTitle(presetPending)} со следующего хода` : undefined}
				error={rejectedPresetDeleted ? 'Инструкции удалены, смена отменена' : (presetsError ?? presetError ?? undefined)}
			/>
			<p class="note">{presetChangeNote(provider)}</p>
		</div>

		<p class="note">Текущий ход продолжится с прежними настройками.</p>

		<section class="crunch">
			<div class="crunch-head">
				<div class="grow">
					<h3>Кранч</h3>
					<p class="note">Продолжать после восстановления лимитов</p>
				</div>
				<Toggle label="Кранч" hideLabel bind:checked={crunchEnabled} />
			</div>
			<Textarea
				id="resume"
				name="resume"
				label="Текст продолжения"
				rows={3}
				bind:value={resumeText}
				disabled={saving}
				hint="Отправится один раз после паузы по лимиту."
			/>
		</section>

		<footer class="footer-action">
			<Button variant="primary" full type="submit">{saving ? 'Сохранение…' : 'Сохранить настройки'}</Button>
			<p class="note">Модель и усилие тоже изменятся со следующего хода.</p>
		</footer>
	</form>
</div>

<style>
	.settings {
		/* Нижний отступ несёт сама подпись сохранения: под ней прокручиваемому тексту не из чего просвечивать. */
		--end: calc(var(--s8) + env(safe-area-inset-bottom));
		display: flex;
		flex-direction: column;
		min-width: 0;
		min-height: 0;
		height: 100%;
		color: var(--text);
	}
	.pane-bar {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--s4);
		margin: var(--s5) var(--gutter) var(--s3);
		padding-block: var(--s2);
	}
	.titles {
		min-width: 0;
	}
	.pane-bar h2 {
		font-size: var(--fs-title);
	}
	.pane-bar p {
		overflow: hidden;
		color: var(--muted);
		font-size: var(--fs-sm);
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.settings-body {
		flex: 1;
		display: flex;
		flex-direction: column;
		gap: var(--s6);
		min-height: 0;
	}
	.settings.desktop { --end: var(--s6); }
	.desktop .settings-body {
		overflow-y: auto;
		padding: var(--s3) var(--gutter) var(--s0);
	}
	.card {
		--field-bg: var(--selected);
		display: flex;
		flex-direction: column;
		gap: var(--s3);
		padding: var(--s6);
		border-radius: var(--radius-card);
		background: var(--raised);
	}
	.state {
		display: flex;
		align-items: center;
		gap: var(--s3);
		margin: 0;
	}
	strong {
		font-weight: var(--weight-semibold);
	}
	.live-dot {
		flex: none;
		width: var(--mark);
		height: var(--mark);
		border-radius: var(--radius-round);
		background: var(--muted);
	}
	.live-dot.active {
		background: var(--accent);
	}
	.note {
		margin: 0;
		color: var(--secondary);
		font-size: var(--fs-meta);
		overflow-wrap: anywhere;
	}
	.note strong {
		color: var(--text);
	}
	.pair {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(min(100%, var(--gauge-width)), 1fr));
		gap: var(--s4);
	}
	.alert {
		padding: var(--s4) var(--s6);
		border-radius: var(--radius-sm);
		background: var(--danger-soft);
		color: var(--danger);
		font-size: var(--fs-meta);
	}
	h3 {
		margin: 0;
		font-size: var(--fs-body);
		font-weight: var(--weight-semibold);
	}
	.crunch {
		display: flex;
		flex-direction: column;
		gap: var(--s4);
	}
	.crunch-head {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--s4);
	}
	.grow {
		flex: 1;
		min-width: 0;
	}
	.footer-action {
		--fade: var(--surface);
		display: flex;
		flex-direction: column;
		gap: var(--s3);
		position: sticky;
		bottom: var(--s0);
		margin-top: auto;
		padding-block: var(--s4) calc(var(--s4) + var(--end));
		background: var(--fade);
	}
	.footer-action::before {
		content: '';
		position: absolute;
		inset: auto 0 100%;
		height: var(--s8);
		background: linear-gradient(to bottom, var(--transparent), var(--fade));
		pointer-events: none;
	}
	.desktop .footer-action { --fade: var(--chat-bg); }
</style>
