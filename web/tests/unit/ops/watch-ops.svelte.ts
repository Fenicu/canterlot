export function watchOpState(
	ops: ReadonlyMap<string, { state: string }>,
	key: string
): { states: string[]; stop: () => void } {
	const states: string[] = [];
	const stop = $effect.root(() => {
		$effect(() => {
			states.push(ops.get(key)?.state ?? 'absent');
		});
	});
	return { states, stop };
}
