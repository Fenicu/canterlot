// Живая строка задачи для карточек ленты: событие хаба меняет её на месте, а у задачи из истории
// строку один раз читают у хаба, чтобы карточка не застыла на «идёт».
import { getJob } from '#lib/api/client.js';
import type { Runtime } from '#lib/app/runtime.js';
import { getRuntime } from '#lib/app/runtime.js';

const asked = new Set<string>();

/** Рантайм, если карточка стоит внутри приложения; в тестах и на экспорте картинкой его нет. */
export function runtimeOrNull(): Runtime | null {
  try {
    return getRuntime();
  } catch {
    return null;
  }
}

/** Читает задачу у хаба не больше одного раза за жизнь страницы; ошибка снимает отметку. */
export async function fetchJobOnce(runtime: Runtime, id: string): Promise<void> {
  if (!id || asked.has(id) || runtime.machines.jobs.has(id)) return;
  asked.add(id);
  try {
    const { data } = await getJob(id);
    runtime.machines.mergeJobs([data.job]);
  } catch {
    asked.delete(id);
  }
}
