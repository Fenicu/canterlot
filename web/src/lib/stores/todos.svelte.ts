import { SvelteMap } from 'svelte/reactivity';
import type { components } from '#lib/api/generated/openapi.js';

export type TodoList = components['schemas']['TodoListView'];
export type TodoItem = TodoList['items'][number];
export type TodoStatus = TodoItem['status'];

const MAIN_LANE = '';

/** Зеркало плана агента по дорожкам: ключ — блок вызова субагента, основной агент отдельно. */
export class TodosModel {
	readonly lists = new SvelteMap<string, TodoList>();

	apply(list: TodoList): void {
		const key = list.lane_block_id ?? MAIN_LANE;
		const known = this.lists.get(key);
		if (known && known.revision >= list.revision) return;
		this.lists.set(key, list);
	}

	/** Событие могло обогнать снимок: сравниваем ревизии, а не заменяем целиком. */
	ingestSnapshot(lists: TodoList[]): void {
		for (const list of lists) this.apply(list);
	}

	lane(id: string | null): TodoList | undefined {
		return this.lists.get(id ?? MAIN_LANE);
	}

	clear(): void {
		this.lists.clear();
	}
}
