<script lang="ts">
	import type { Activity } from '#lib/screens/sessions/activityText.js';

	export type RequestView = {
		id?: string;
		question?: string;
		[key: string]: unknown;
	};

	let {
		request = null,
		activity = null
	}: {
		request?: RequestView | null;
		activity?: Activity | null;
	} = $props();

	const isWaiting = $derived(
		activity?.state === 'waiting_for_user' || request !== null
	);

	const questionText = $derived(
		request?.question || activity?.evidence?.question || ''
	);
</script>

{#if isWaiting}
	<div class="request-slot-container">
		<section class="question-card" aria-label="Вопрос от агента">
			<div class="card-head">
				<span class="meta">Вопрос от агента</span>
			</div>
			{#if questionText}
				<h2>{questionText}</h2>
			{/if}
			<p class="placeholder-note">
				Ответ из приложения появится в следующем обновлении. Агент ждёт ответа.
			</p>
		</section>
	</div>
{/if}

<style>
	.request-slot-container {
		padding: var(--s3) var(--s4);
		box-sizing: border-box;
		width: 100%;
	}
	.question-card {
		padding: var(--s4);
		border-radius: var(--radius-card);
		background: var(--surface);
		display: flex;
		flex-direction: column;
		gap: var(--s3);
		color: var(--text);
	}
	.card-head {
		display: flex;
		justify-content: space-between;
		align-items: center;
	}
	.meta {
		font-size: var(--fs-meta);
		color: var(--muted);
	}
	h2 {
		margin: 0;
		font-size: var(--fs-title);
		font-weight: var(--weight-semibold);
	}
	.placeholder-note {
		margin: 0;
		font-size: var(--fs-body);
		color: var(--muted);
	}
</style>
