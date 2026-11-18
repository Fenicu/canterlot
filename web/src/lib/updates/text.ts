/** Тексты ленты обновлений CLI и моделей: одна строка на запись, без сырых идентификаторов. */

export type UpdateKind =
	| 'version_available'
	| 'below_minimum'
	| 'cli_updated'
	| 'models_added'
	| 'models_removed';

export interface UpdatePayload {
	installed_version?: string;
	latest_version?: string;
	min_version?: string;
	from?: string;
	to?: string;
	models?: string[];
	update_command?: string;
}

export interface UpdateItem {
	id: string;
	kind: string;
	machine: string;
	harness: string;
	created_at: string;
	payload: UpdatePayload;
	update_command?: string | null;
}

const HARNESS_NAMES: Record<string, string> = {
	claude: 'Claude Code',
	claude_code: 'Claude Code',
	codex: 'Codex',
	opencode: 'OpenCode'
};

const VERSION_KINDS = new Set<string>(['version_available', 'below_minimum', 'cli_updated']);

/** Имя инструмента в ленте совпадает с тем, как PWA называет его в других местах. */
export function updateHarness(harness: string): string {
	return HARNESS_NAMES[harness] ?? harness;
}

/** Длинный список моделей сворачивается до пяти имён и счётчика остатка. */
export function foldModels(models: string[] | undefined, limit = 5): string {
	const list = (Array.isArray(models) ? models : []).filter(
		(value): value is string => typeof value === 'string' && value.trim().length > 0
	);
	if (list.length === 0) return '—';
	const shown = list.slice(0, limit).join(', ');
	return list.length > limit ? `${shown} и ещё ${list.length - limit}` : shown;
}

export function isVersionKind(kind: string): boolean {
	return VERSION_KINDS.has(kind);
}

/** Приводит запись ленты и полезную нагрузку живого события к одному виду. */
export function toUpdateItem(raw: Record<string, unknown>): UpdateItem {
	const payload = (raw.payload && typeof raw.payload === 'object' && !Array.isArray(raw.payload)
		? raw.payload
		: {}) as UpdatePayload;
	const command = typeof raw.update_command === 'string' ? raw.update_command : payload.update_command;
	return {
		id: String(raw.id ?? ''),
		kind: String(raw.kind ?? ''),
		machine: String(raw.machine ?? ''),
		harness: String(raw.harness ?? ''),
		created_at: String(raw.created_at ?? ''),
		payload,
		update_command: typeof command === 'string' ? command : null
	};
}

/** Ниже поддерживаемой версии — тревога, остальные записи нейтральны. */
export function updateTone(kind: string): 'warning' | 'neutral' {
	return kind === 'below_minimum' ? 'warning' : 'neutral';
}

/** Команда приходит полем ответа ленты или внутри полезной нагрузки живого события. */
export function updateCommand(item: UpdateItem): string {
	return item.update_command ?? item.payload.update_command ?? '';
}

export function updateText(item: UpdateItem): string {
	const harness = updateHarness(item.harness);
	const installed = item.payload.installed_version ?? '';
	switch (item.kind) {
		case 'version_available':
			return `Доступна ${harness} ${item.payload.latest_version || 'новая версия'}${installed ? ` (установлена ${installed})` : ''}`;
		case 'below_minimum':
			return `Ниже поддерживаемой версии: ${harness} ${installed || 'неизвестная'}, нужна ${item.payload.min_version || 'поддерживаемая'}`;
		case 'cli_updated':
			return `${harness} обновился сам: ${item.payload.from || 'неизвестная'} → ${item.payload.to || 'неизвестная'}`;
		case 'models_added':
			return `Добавлены модели: ${foldModels(item.payload.models)}`;
		case 'models_removed':
			return `Убраны модели: ${foldModels(item.payload.models)}`;
		default:
			return 'Новое обновление';
	}
}
