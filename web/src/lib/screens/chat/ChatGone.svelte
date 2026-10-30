<script lang="ts">
	import { resolve } from '$app/paths';

	let { reason }: { reason: 'deleted' | 'unavailable' } = $props();
</script>

<section class="gone">
	<h1>{reason === 'deleted' ? 'Сессия удалена' : 'Сессия не найдена'}</h1>
	<p>
		{reason === 'deleted'
			? 'Её история и вложения больше не хранятся на хабе.'
			: 'Возможно, она удалена или открыта по устаревшей ссылке.'}
	</p>
	<a href={resolve('sessions')}>К списку сессий</a>
</section>

<style>
	.gone {
		display: flex;
		flex: 1;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		gap: var(--s3);
		padding: var(--s8) var(--gutter);
		text-align: center;
	}
	h1 {
		margin: 0;
		font-size: var(--fs-title);
		font-weight: var(--weight-semibold);
	}
	p {
		margin: 0;
		color: var(--secondary);
	}
	a {
		display: inline-flex;
		align-items: center;
		min-height: var(--target);
		color: var(--accent-link);
	}
</style>
