/** Живой ход карточки в тесте: проп `activeRun` читается через геттер и меняется по ходу теста. */
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
