export interface RunInfo {
	run_id: string;
	input_id: string;
	started_at: string;
	finished_at?: string | null;
	duration_ms?: number | null;
	parts: { block_id: string; kind: string; agent: string; model: string }[];
	usage: unknown[];
}

export class RunStore {
	#cache = new Map<string, RunInfo>();

	constructor(private loadRun: (runId: string) => Promise<RunInfo>) {}

	async load(runId: string): Promise<RunInfo> {
		const cached = this.#cache.get(runId);
		if (cached) return cached;
		const info = await this.loadRun(runId);
		this.#cache.set(runId, info);
		return info;
	}

	invalidate(runId: string): void {
		this.#cache.delete(runId);
	}
}
