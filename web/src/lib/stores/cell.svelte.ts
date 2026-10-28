export interface Cell<T> {
	get(): T;
	set(value: T): void;
}

export function cell<T>(initial: T): Cell<T> {
	let value = $state(initial);
	return {
		get: () => value,
		set: (next: T) => {
			value = next;
		}
	};
}

export function rawCell<T>(initial: T): Cell<T> {
	let value = $state.raw(initial);
	return {
		get: () => value,
		set: (next: T) => {
			value = next;
		}
	};
}
