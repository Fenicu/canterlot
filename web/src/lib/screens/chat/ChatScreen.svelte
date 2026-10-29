<script lang="ts">
	import { onMount } from 'svelte';
	import { browserRuntime } from './open';
	import { getRuntime, type Runtime } from '#lib/app/runtime.js';
	import { loadConfig } from '#lib/api/config.js';
	import Composer from '#lib/composer/Composer.svelte';
	import type { ChatStore } from '#lib/stores/chat.svelte.js';
	import PendingStrip from '#lib/timeline/PendingStrip.svelte';
	import Timeline from '#lib/timeline/Timeline.svelte';
	import ChatHeader from './ChatHeader.svelte';
	import PauseCards from './PauseCards.svelte';
	import RequestCard from '#lib/requests/RequestCard.svelte';
	import type { RequestItem } from '#lib/stores/requests.svelte.js';
	import SessionSettings from './SessionSettings.svelte';
	import Diagnostics from './Diagnostics.svelte';
	import InfoSheet from './InfoSheet.svelte';
	import type { ProviderLimits, LimitThresholds } from '#lib/stores/limits.js';
	import type { MachineRecord } from '#lib/stores/machines.svelte.js';
	import Sheet from '#lib/ui/Sheet.svelte';

	interface MachineRef {
		id?: string;
	}

	interface ChatDetailWithMachine {
		machine?: MachineRef;
	}

	let {
		chat,
		onInfo,
		now = Date.now(),
		providers = [],
		thresholds = { warn_pcts: [80, 90], weekly_reserve_pct: 95, stale_after_secs: 1800 },
		machine = null
	}: {
		chat: ChatStore;
		onInfo?: (runId: string) => void;
		now?: number;
		providers?: ProviderLimits[];
		thresholds?: LimitThresholds;
		machine?: MachineRecord | null;
	} = $props();

	let runtime = $state<Runtime | null>(null);
	try {
		const rt = getRuntime();
		rt.machines.open();
		runtime = rt;
	} catch {
		// runtime singleton not yet created
	}

	onMount(() => {
		browserRuntime().then((rt) => {
			runtime = rt;
			rt.machines.open();
		});
	});

	const machineId = $derived((chat.detail as ChatDetailWithMachine | null)?.machine?.id);
	const resolvedMachine = $derived(
		machine ?? (runtime && machineId ? (runtime.machines.items.get(machineId) ?? null) : null)
	);

	let showSettings = $state(false);
	let showDiagnostics = $state(false);
	let infoRunId = $state<string | null>(null);

	onMount(async () => {
		try {
			const runtime = await browserRuntime();
			const detail = chat.detail as ChatDetailWithMachine | null;
			if (!machine && detail?.machine?.id) {
				const mid = detail.machine.id;
				machine = runtime.machines.items.get(mid) ?? [...runtime.machines.items.values()][0] ?? null;
			}
			if (providers.length === 0 && runtime.list.limits.length > 0) {
				providers = runtime.list.limits;
			}
			const cfg = await loadConfig();
			if (cfg?.limit_thresholds) {
				thresholds = cfg.limit_thresholds;
			}
		} catch {
			// fallback
		}
	});

	const activityRequest = $derived.by((): RequestItem | null => {
		const evidence = chat.activity?.evidence as Record<string, unknown> | undefined;
		if (chat.activity?.state === 'waiting_for_user' && typeof evidence?.question === 'string') {
			return {
				id: 'activity-request',
				conversationId: chat.id,
				kind: 'question',
				state: 'open',
				payload: {
					questions: [
						{
							id: 'q0',
							text: evidence.question
						}
					]
				}
			};
		}
		return null;
	});
</script>

<section class="screen" data-ready>
	<ChatHeader
		{chat}
		{providers}
		{thresholds}
		{now}
		onOpenSettings={() => (showSettings = true)}
		onOpenDiagnostics={() => (showDiagnostics = true)}
	/>

	<Timeline
		{chat}
		onInfo={(runId) => {
			infoRunId = runId;
			onInfo?.(runId);
		}}
		{now}
	/>

	{#if chat.requests.items.size > 0}
		<div class="request-cards-slot">
			{#each [...chat.requests.items.values()] as req (req.id)}
				<RequestCard
					request={req}
					onDecision={async (decision) => {
						await chat.requests.submitDecision(req.id, decision, chat.ops);
					}}
				/>
			{/each}
		</div>
	{:else if activityRequest}
		<div class="request-card-slot">
			<RequestCard
				request={activityRequest}
				onDecision={async (decision) => {
					await chat.requests.submitDecision(activityRequest.id, decision, chat.ops);
				}}
			/>
		</div>
	{/if}

	<PauseCards {chat} {now} />

	<PendingStrip {chat} />

	<Composer {chat} />

	{#if showSettings}
		<Sheet open title="Настройки сессии" onclose={() => (showSettings = false)}>
			<SessionSettings
				{chat}
				machine={resolvedMachine}
				onclose={() => (showSettings = false)}
			/>
		</Sheet>
	{/if}

	{#if showDiagnostics}
		<Diagnostics
			{chat}
			onclose={() => (showDiagnostics = false)}
		/>
	{/if}

	{#if infoRunId}
		<InfoSheet
			{chat}
			runId={infoRunId}
			onclose={() => (infoRunId = null)}
		/>
	{/if}
</section>

<style>
	.screen {
		flex: 1;
		height: auto;
		display: flex;
		flex-direction: column;
		min-width: 0;
		min-height: 0;
		position: relative;
	}

	.request-cards-slot,
	.request-card-slot {
		padding: var(--s3) var(--s4);
		box-sizing: border-box;
		width: 100%;
		display: flex;
		flex-direction: column;
		gap: var(--s3);
	}
</style>
