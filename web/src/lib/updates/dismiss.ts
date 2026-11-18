/** Закрытие баннера обновлений: отметка хранится на устройстве отдельно для каждого владельца. */

export interface DismissMarker {
	id: string;
	created_at: string;
}

export interface DismissStorage {
	getItem(key: string): string | null;
	setItem(key: string, value: string): void;
}

export function dismissKey(ownerId: string): string {
	return `canterlot.updates.dismissed.${ownerId}`;
}

export function readDismiss(ownerId: string, storage: DismissStorage): DismissMarker | null {
	try {
		const raw = storage.getItem(dismissKey(ownerId));
		if (!raw) return null;
		const parsed: unknown = JSON.parse(raw);
		if (!parsed || typeof parsed !== 'object') return null;
		const value = parsed as Record<string, unknown>;
		return typeof value.id === 'string' && value.id.length > 0 &&
			typeof value.created_at === 'string' && value.created_at.length > 0
			? { id: value.id, created_at: value.created_at }
			: null;
	} catch {
		return null;
	}
}

export function writeDismiss(ownerId: string, storage: DismissStorage, marker: DismissMarker): void {
	try {
		storage.setItem(dismissKey(ownerId), JSON.stringify(marker));
	} catch {
		// Закрытие останется до перезагрузки: недоступное хранилище не должно ломать экран.
	}
}

/** Порядок ленты: время, а при равном времени — идентификатор записи. */
export function compareUpdates(a: DismissMarker, b: DismissMarker): number {
	const at = Date.parse(a.created_at);
	const bt = Date.parse(b.created_at);
	if (Number.isFinite(at) && Number.isFinite(bt) && at !== bt) return at - bt;
	if (a.created_at !== b.created_at) return a.created_at < b.created_at ? -1 : 1;
	if (a.id === b.id) return 0;
	return a.id < b.id ? -1 : 1;
}

export function isNewer(item: DismissMarker, marker: DismissMarker | null): boolean {
	return marker === null || compareUpdates(item, marker) > 0;
}

export function unseenUpdates<T extends DismissMarker>(items: T[], marker: DismissMarker | null): T[] {
	return items.filter((item) => isNewer(item, marker));
}

export function shouldShowBanner(items: DismissMarker[], marker: DismissMarker | null): boolean {
	return unseenUpdates(items, marker).length > 0;
}

/** Отметка ставится на самую свежую запись: всё, что не новее её, считается просмотренным. */
export function newestDismissible(items: DismissMarker[]): DismissMarker | null {
	return items.length > 0 ? { id: items[0]!.id, created_at: items[0]!.created_at } : null;
}
