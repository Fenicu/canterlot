import { SvelteMap } from 'svelte/reactivity';
import type { components } from '#lib/api/generated/openapi.js';

export type BackgroundTask = components['schemas']['BackgroundTaskView'];

/** Что ленте и делегированию нужно знать о фоновой задаче вызова. */
export interface BackgroundFact {
	state: string;
	kind: string;
	endedAt: number | null;
}

/** Зеркало списка хаба: по записи на задачу, `task_id` может работать снова после конца. */
export class BackgroundModel {
	readonly tasks = new SvelteMap<string, BackgroundTask>();

	apply(tasks: BackgroundTask[]): void {
		for (const task of tasks) this.tasks.set(task.task_id, task);
	}

	/** Событие могло обогнать снимок: конец и новый запуск из события снимок не отменяет. */
	ingestSnapshot(tasks: BackgroundTask[]): void {
		for (const task of tasks) {
			const known = this.tasks.get(task.task_id);
			if (known && (known.started_at > task.started_at || (known.ended_at && !task.ended_at))) continue;
			this.tasks.set(task.task_id, task);
		}
	}

	/** Задачи по блокам вызовов. Живой строки у остановившегося исполнения нет: процесса уже нет. */
	facts(live: boolean): Map<string, BackgroundFact> {
		const facts = new Map<string, BackgroundFact>();
		for (const task of this.tasks.values()) {
			if (!task.block_id) continue;
			const ended = task.ended_at ? Date.parse(task.ended_at) : null;
			facts.set(task.block_id, {
				kind: task.kind,
				state: task.state === 'running' && !live ? 'stopped' : task.state,
				endedAt: ended
			});
		}
		return facts;
	}

	clear(): void {
		this.tasks.clear();
	}
}
