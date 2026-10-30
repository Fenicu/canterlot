export function countEffectRuns(fn: () => void): {
  runs(): number;
  stop(): void;
} {
  let runs = 0;
  const stop = $effect.root(() => {
    $effect(() => {
      fn();
      runs += 1;
    });
  });
  return { runs: () => runs, stop };
}
