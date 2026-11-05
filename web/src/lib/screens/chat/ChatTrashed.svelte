<script lang="ts">
  import { resolve } from '$app/paths';
  import { restoreConversation } from '#lib/api/client.js';
  import { errorText } from '#lib/api/errors.js';
  import Button from '#lib/ui/Button.svelte';

  let { id }: { id: string } = $props();
  let busy = $state(false);
  let error = $state('');

  async function restore(): Promise<void> {
    busy = true;
    error = '';
    try {
      await restoreConversation(id, crypto.randomUUID());
    } catch (failure) {
      error = errorText(failure, 'Не удалось вернуть сессию');
    } finally {
      busy = false;
    }
  }
</script>

<section class="trashed">
  <h1>Сессия в корзине</h1>
  <p>Её можно вернуть в течение 24 часов. Потом история и вложения удалятся окончательно.</p>
  <Button busy={busy} onclick={() => void restore()}>Восстановить</Button>
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  <a href={resolve('sessions')}>К списку сессий</a>
</section>

<style>
  .trashed {
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
    max-width: 90%;
    margin: 0;
    color: var(--secondary);
  }
  .error { color: var(--danger); }
  a {
    display: inline-flex;
    align-items: center;
    min-height: var(--target);
    color: var(--accent-link);
  }
</style>
