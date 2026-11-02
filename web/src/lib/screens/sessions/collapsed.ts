const KEY = 'canterlot.collapsed';
const KIDS_KEY = 'canterlot.kids';

export function readCollapsed(): string[] {
	try {
		const raw = localStorage.getItem(KEY);
		const parsed = raw ? (JSON.parse(raw) as unknown) : [];
		return Array.isArray(parsed) ? parsed.filter((item) => typeof item === 'string') : [];
	} catch {
		return [];
	}
}

export function writeCollapsed(ids: readonly string[]): void {
	localStorage.setItem(KEY, JSON.stringify(ids));
}

export function collapsedSet(ids: readonly string[]): ReadonlySet<string> {
	return new Set(ids);
}

/** Явный выбор по родителям делегированных сессий: что свернули, что раскрыли. */
export interface KidsChoice {
	collapsed: string[];
	expanded: string[];
}

export function readKidsChoice(): KidsChoice {
	try {
		const raw = localStorage.getItem(KIDS_KEY);
		const parsed = raw ? (JSON.parse(raw) as unknown) : null;
		if (parsed && typeof parsed === 'object') {
			const record = parsed as Record<string, unknown>;
			const pick = (key: string): string[] =>
				Array.isArray(record[key]) ? (record[key] as unknown[]).filter((item): item is string => typeof item === 'string') : [];
			return { collapsed: pick('collapsed'), expanded: pick('expanded') };
		}
	} catch {
		// битое хранилище читается как пустой выбор
	}
	return { collapsed: [], expanded: [] };
}

export function writeKidsChoice(choice: KidsChoice): void {
	localStorage.setItem(KIDS_KEY, JSON.stringify(choice));
}
