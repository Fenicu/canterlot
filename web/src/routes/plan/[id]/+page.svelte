<script lang="ts">
  import { page } from '$app/state';
  import PlanScreen from '#lib/screens/plan/PlanScreen.svelte';

  const id = $derived(page.params.id ?? '');
</script>
<svelte:head><title>План · Canterlot</title></svelte:head>
{#key id}<PlanScreen orchestratorId={id} />{/key}
