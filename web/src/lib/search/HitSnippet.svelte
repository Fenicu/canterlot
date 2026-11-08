<script lang="ts">
	import { cleanSegments, type SearchSegment } from './model';

	let { segments }: { segments: SearchSegment[] } = $props();
	const parts = $derived(cleanSegments(segments));
</script>

<!-- Совпавшие слова выделены разметкой mark: текст сниппета никогда не вставляется как HTML. -->
<span class="snippet" data-search-snippet
	>{#each parts as part, index (index)}{#if part.hit}<mark>{part.text}</mark>{:else}{part.text}{/if}{/each}</span
>

<style>
	.snippet {
		display: -webkit-box;
		-webkit-box-orient: vertical;
		-webkit-line-clamp: 3;
		line-clamp: 3;
		overflow: hidden;
		color: var(--text);
		font-size: var(--fs-meta);
		line-height: var(--lh-body);
		overflow-wrap: anywhere;
	}
	mark {
		padding-inline: var(--s1);
		border-radius: var(--radius-sm);
		background: var(--accent-soft);
		color: var(--text);
		font-weight: var(--weight-semibold);
		box-decoration-break: clone;
		-webkit-box-decoration-break: clone;
	}
</style>
