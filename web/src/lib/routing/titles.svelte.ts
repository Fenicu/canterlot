// Русские названия типов задач для строк маршрута: политика читается один раз
// на ревизию и перечитывается по `routing_policy_changed`.
import { getRoutingPolicy } from '#lib/api/client.js';

type TaskTypes = Record<string, { title?: string }>;

class RoutingTitles {
  #titles = $state<Record<string, string>>({});
  #revision: number | null | undefined = undefined;

  /** Перечитывает политику, если ревизия изменилась; ошибки оставляют ключи. */
  ensure(revision: number | null): void {
    if (this.#revision === revision) return;
    this.#revision = revision;
    getRoutingPolicy()
      .then((answer) => {
        const policy = (answer.data.policy ?? answer.data.builtin) as { task_types?: TaskTypes } | null;
        const next: Record<string, string> = {};
        for (const [key, task] of Object.entries(policy?.task_types ?? {})) if (task.title) next[key] = task.title;
        this.#titles = next;
      })
      .catch(() => {
        this.#revision = undefined;
      });
  }

  title(key: string): string {
    return this.#titles[key] ?? key;
  }
}

export const routingTitles = new RoutingTitles();
