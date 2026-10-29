export interface DiagnosticsData {
	conversationId: string;
	connectionId?: string | null;
	execution?: {
		id?: string | null;
		state?: string | null;
		generation?: number | null;
		outcome?: string | null;
	} | null;
	activity?: {
		state?: string | null;
		evidence?: Record<string, unknown> | null;
	} | null;
	scope?: {
		cursor?: string | null;
		status?: string | null;
		resyncCount?: number | null;
		lastFrameAgeMs?: number | null;
	} | null;
	versions?: {
		build?: string | null;
		protocol?: number | null;
	} | null;
	diagnosticEvents?: Array<{
		kind: string;
		correlation_id?: string;
		seq?: string;
	}>;
}

export function buildDiagnosticsReport(data: DiagnosticsData): string {
	const safe = {
		conversationId: data.conversationId,
		connectionId: data.connectionId ?? 'none',
		execution: {
			id: data.execution?.id ?? null,
			state: data.execution?.state ?? null,
			generation: data.execution?.generation ?? null,
			outcome: data.execution?.outcome ?? null
		},
		activity: {
			state: data.activity?.state ?? null,
			phase: (data.activity?.evidence?.phase as string) ?? null
		},
		scope: {
			cursor: data.scope?.cursor ?? null,
			status: data.scope?.status ?? null,
			resyncCount: data.scope?.resyncCount ?? 0,
			lastFrameAgeMs: data.scope?.lastFrameAgeMs ?? null
		},
		versions: {
			build: data.versions?.build ?? '1.0.0',
			protocol: data.versions?.protocol ?? 1
		},
		diagnosticEvents: (data.diagnosticEvents ?? []).map((e) => ({
			kind: e.kind,
			correlation_id: e.correlation_id ?? null,
			seq: e.seq ?? null
		}))
	};

	return [
		`=== Canterlot Diagnostics Report ===`,
		`conversationId: ${safe.conversationId}`,
		`connectionId: ${safe.connectionId}`,
		`execution.id: ${safe.execution.id ?? 'none'}`,
		`execution.state: ${safe.execution.state ?? 'none'}`,
		`execution.generation: ${safe.execution.generation ?? 'none'}`,
		`execution.outcome: ${safe.execution.outcome ?? 'none'}`,
		`activity.state: ${safe.activity.state ?? 'none'}`,
		`activity.phase: ${safe.activity.phase ?? 'none'}`,
		`scope.cursor: ${safe.scope.cursor ?? 'none'}`,
		`scope.status: ${safe.scope.status ?? 'none'}`,
		`scope.resyncCount: ${safe.scope.resyncCount}`,
		`scope.lastFrameAgeMs: ${safe.scope.lastFrameAgeMs ?? 'none'}`,
		`versions.build: ${safe.versions.build}`,
		`versions.protocol: ${safe.versions.protocol}`,
		`diagnosticEvents:`,
		...safe.diagnosticEvents.map((e) => `  - ${e.kind} (${e.correlation_id ?? 'no-corr-id'}) [seq: ${e.seq ?? 'none'}]`)
	].join('\n');
}
