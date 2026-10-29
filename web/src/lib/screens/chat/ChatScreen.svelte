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
	import RequestSlot, { type RequestView } from './RequestSlot.svelte';
	import SessionSettings from './SessionSettings.svelte';
	import Diagnostics from './Diagnostics.svelte';
	import InfoSheet from './InfoSheet.svelte';
	import type { ProviderLimits, LimitThresholds } from '#lib/stores/limits.js';
	import type { MachineRecord } from '#lib/stores/machines.svelte.js';
	import type { Activity } from '#lib/screens/sessions/activityText.js';
	import Sheet from '#lib/ui/Sheet.svelte';

	interface MachineRef {
		id?: string;
	}

	interface ChatDetailWithMachine {
		machine?: MachineRef;
	}

	interface RequestEventLike {
		kind?: string;
		payload?: RequestView;
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

	const activeRequest = $derived.by(() => {
		const reqs = chat.requests ?? [];
		for (let i = reqs.length - 1; i >= 0; i--) {
			const r = reqs[i] as RequestEventLike | undefined;
			if (r?.kind === 'request_closed' || r?.kind === 'request_decided') return null;
			if (r?.kind === 'request_opened') return r.payload ?? null;
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

	<RequestSlot request={activeRequest} activity={chat.activity as Activity | null} />

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
</style>
