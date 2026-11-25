/**
 * Живой ход карточки в тесте: проп `activeRun` читается через геттер и меняется по ходу теста.
 * Карточка получает команду хода (`input_command_id` её блоков), а не id ввода из слота очереди.
 */
export function liveRun(start: string | null): { readonly activeRun: string | null; stop(): void } {
  let activeRun = $state(start);
  return {
    get activeRun() {
      return activeRun;
    },
    stop() {
      activeRun = null;
    },
  };
}

/**
 * Активность и слот очереди идущего хода для ленты. Слот называет ввод (`input`), а блоки хода
 * несут его команду (`command`): это разные id, как у хаба. Без `input` ход начал сам CLI и слота нет.
 */
export function liveSlot(opts: { input: string | null; command: string | null; since?: string; state?: string; phase?: string; tool?: string }) {
  const since = opts.since ?? "2026-11-25T09:58:00.000Z";
  let activity = $state<Record<string, unknown> | null>({
    state: opts.state ?? "working",
    since,
    evidence: { phase: opts.phase ?? "running_tool", tool_name: opts.tool ?? "Bash", phase_since: since },
  });
  let runSlot = $state<unknown>(
    opts.input === null ? "free" : { running: opts.command === null ? { input_id: opts.input } : { input_id: opts.input, command_id: opts.command } },
  );
  return {
    get activity() {
      return activity;
    },
    get queue() {
      return { queued: 0, pause_reasons: [], hold_reasons: [], run_slot: runSlot };
    },
    /** Активность меняется без нового блока: другое состояние или фаза сдвигают `since`. */
    shift(next: { state?: string; since: string; phase?: string }) {
      activity = {
        state: next.state ?? "working",
        since: next.since,
        evidence: { phase: next.phase ?? opts.phase ?? "running_tool", tool_name: opts.tool ?? "Bash", phase_since: next.since },
      };
    },
    stop() {
      activity = { state: "idle", since };
      runSlot = "free";
    },
  };
}

