import { SvelteMap } from 'svelte/reactivity';
import type { PlanTask } from '#lib/api/generated/events.js';
import type { components } from '#lib/api/generated/openapi.js';

export type { PlanTask };
export type PlanTaskState = PlanTask['state'];
export type PlanTaskDetail = components['schemas']['PlanTaskDetail'];

/**
 * Задачи планов всех оркестраторов владельца. Снимок списка заменяет набор целиком,
 * событие `plan_task_changed` правит одну задачу и не откатывает её к старой ревизии.
 */
export class PlanModel {
	readonly tasks = new SvelteMap<string, PlanTask>();
	/** Растёт с каждой принятой правкой задачи: экран плана по нему перечитывает подробности. */
	readonly changes = new SvelteMap<string, number>();

	apply(task: PlanTask): boolean {
		const known = this.tasks.get(task.id);
		if (known && known.revision >= task.revision) return false;
		this.tasks.set(task.id, task);
		this.#touch(task.orchestrator_id);
		return true;
	}

	replace(tasks: readonly PlanTask[]): void {
		const touched = [...this.tasks.values(), ...tasks].map((task) => task.orchestrator_id);
		this.tasks.clear();
		for (const task of tasks) this.tasks.set(task.id, task);
		for (const id of touched.filter((value, index) => touched.indexOf(value) === index)) this.#touch(id);
	}

	/** Оркестратор удалён: его план уходит вместе с ним. */
	dropOrchestrator(id: string): void {
		for (const task of [...this.tasks.values()]) if (task.orchestrator_id === id) this.tasks.delete(task.id);
		this.changes.delete(id);
	}

	/** Задачи одного плана по номерам. */
	of(orchestratorId: string): PlanTask[] {
		return [...this.tasks.values()].filter((task) => task.orchestrator_id === orchestratorId).sort((a, b) => a.number - b.number);
	}

	has(orchestratorId: string): boolean {
		for (const task of this.tasks.values()) if (task.orchestrator_id === orchestratorId) return true;
		return false;
	}

	/** Оркестраторы, у которых есть план: последний изменённый первым. */
	orchestrators(): string[] {
		const latest: Record<string, string> = {};
		for (const task of this.tasks.values()) {
			const known = latest[task.orchestrator_id];
			if (!known || task.updated_at > known) latest[task.orchestrator_id] = task.updated_at;
		}
		return Object.entries(latest).sort((a, b) => b[1].localeCompare(a[1]) || a[0].localeCompare(b[0])).map(([id]) => id);
	}

	revisionOf(orchestratorId: string): number {
		return this.changes.get(orchestratorId) ?? 0;
	}

	clear(): void {
		this.tasks.clear();
		this.changes.clear();
	}

	#touch(id: string): void {
		this.changes.set(id, (this.changes.get(id) ?? 0) + 1);
	}
}
