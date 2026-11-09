import { SvelteMap } from 'svelte/reactivity';
import type { PlanTask } from '#lib/api/generated/events.js';
import type { components } from '#lib/api/generated/openapi.js';

export type { PlanTask };
export type PlanTaskState = PlanTask['state'];
export type PlanTaskDetail = components['schemas']['PlanTaskDetail'];

/**
 * Задачи планов всех оркестраторов владельца. Снимок списка заменяет набор целиком,
 * событие `plan_task_changed` правит одну задачу и не откатывает её к старой ревизии.
 *
 * Снимок неполон (без архива, планов делегатов и старых готовых задач), поэтому план,
 * прочитанный экраном плана по HTTP, хранится отдельно и служит основой: живая задача
 * накладывается на него, если её ревизия не меньше.
 */
export class PlanModel {
	readonly tasks = new SvelteMap<string, PlanTask>();
	readonly #loaded = new SvelteMap<string, readonly PlanTask[]>();
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

	/** План, прочитанный по HTTP (`GET /api/v1/plan`): правда хаба на момент чтения. */
	loaded(orchestratorId: string, tasks: readonly PlanTask[]): void {
		// Задача той же ревизии не менялась: прежний объект остаётся, и строка плана не пересчитывается.
		const known: Record<string, PlanTask> = {};
		for (const task of this.#loaded.get(orchestratorId) ?? []) known[task.id] = task;
		this.#loaded.set(
			orchestratorId,
			tasks.map((task) => {
				const prev = known[task.id];
				return prev && prev.revision === task.revision ? prev : task;
			})
		);
	}

	/** Оркестратор удалён: его план уходит вместе с ним. */
	dropOrchestrator(id: string): void {
		for (const task of [...this.tasks.values()]) if (task.orchestrator_id === id) this.tasks.delete(task.id);
		this.#loaded.delete(id);
		this.changes.delete(id);
	}

	/** Задачи одного плана по номерам: прочитанный план с наложенными живыми задачами. */
	of(orchestratorId: string): PlanTask[] {
		const merged: Record<string, PlanTask> = {};
		for (const task of this.#loaded.get(orchestratorId) ?? []) {
			const live = this.tasks.get(task.id);
			merged[task.id] = live && live.revision >= task.revision ? live : task;
		}
		for (const task of this.tasks.values()) if (task.orchestrator_id === orchestratorId && !(task.id in merged)) merged[task.id] = task;
		return Object.values(merged).filter((task) => task.orchestrator_id === orchestratorId).sort((a, b) => a.number - b.number);
	}

	has(orchestratorId: string): boolean {
		for (const task of this.tasks.values()) if (task.orchestrator_id === orchestratorId) return true;
		return this.#loaded.has(orchestratorId) && this.of(orchestratorId).length > 0;
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
		this.#loaded.clear();
		this.changes.clear();
	}

	#touch(id: string): void {
		this.changes.set(id, (this.changes.get(id) ?? 0) + 1);
	}
}
