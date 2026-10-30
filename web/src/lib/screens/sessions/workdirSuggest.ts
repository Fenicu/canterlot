import { workdirProblem } from "./newSessionModel";

export const SUGGEST_DEBOUNCE_MS = 200;

export type SuggestLoad = (
  machineId: string,
  path: string,
  signal: AbortSignal,
) => Promise<string[]>;

/** Подсказки необязательны: отказ, срок и поздний ответ только убирают список. */
export class WorkdirSuggester {
  #input = 0;
  #timer: ReturnType<typeof setTimeout> | undefined;
  #running: AbortController | null = null;
  #items: string[] = [];

  constructor(
    private load: SuggestLoad,
    private show: (items: string[]) => void,
  ) {}

  input(machineId: string, value: string, wait = SUGGEST_DEBOUNCE_MS): void {
    this.#cancel();
    const path = value.trim();
    if (!machineId || !path || workdirProblem(path)) {
      this.#publish([]);
      return;
    }
    this.#publish(this.#items.filter((item) => item.startsWith(path)));
    const input = this.#input;
    this.#timer = setTimeout(() => void this.#run(input, machineId, path), wait);
  }

  clear(): void {
    this.#cancel();
    this.#publish([]);
  }

  async #run(input: number, machineId: string, path: string): Promise<void> {
    const running = new AbortController();
    this.#running = running;
    const items = await this.load(machineId, path, running.signal).catch(
      () => [],
    );
    if (input === this.#input) this.#publish(items);
  }

  #cancel(): void {
    this.#input += 1;
    clearTimeout(this.#timer);
    this.#running?.abort();
    this.#running = null;
  }

  #publish(items: string[]): void {
    this.#items = items;
    this.show(items);
  }
}
