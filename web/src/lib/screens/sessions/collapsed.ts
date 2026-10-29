const KEY = 'canterlot.collapsed';

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
